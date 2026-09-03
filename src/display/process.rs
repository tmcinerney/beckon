use std::{
    process::Stdio,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, ChildStdin, Command},
    runtime::Builder,
    sync::watch,
    time::{MissedTickBehavior, interval, sleep, timeout},
};

use crate::{
    config::ProcessOutputPlugin,
    display::{DisplaySink, PublishOutcome},
    display_protocol::DisplayMessage,
    render::RenderPlan,
};

const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const RESTART_DELAY: Duration = Duration::from_secs(1);
const EXIT_POLL_INTERVAL: Duration = Duration::from_secs(1);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

#[derive(Clone)]
struct QueuedRender {
    sequence: u64,
    bytes: Arc<[u8]>,
}

enum WorkerStatus {
    Delivered,
    Failed(String),
}

/// A supervised, out-of-process display plugin.
///
/// `publish` only serializes and replaces the worker's latest snapshot. Child
/// process startup and I/O remain off Tao's main-thread hotkey event loop.
pub(super) struct ProcessDisplay {
    id: String,
    updates: Option<watch::Sender<Option<QueuedRender>>>,
    statuses: Receiver<WorkerStatus>,
    worker: Option<JoinHandle<()>>,
    last_plan: Option<RenderPlan>,
    next_sequence: u64,
}

impl ProcessDisplay {
    pub(super) fn new(plugin: ProcessOutputPlugin) -> Result<Self> {
        let id = plugin.id.clone();
        // AIDEV-NOTE: A watch channel is a one-item, latest-state mailbox. It
        // bounds memory and keeps a blocked plugin off Tao's hotkey thread.
        let (updates, receiver) = watch::channel(None);
        let (status_sender, statuses) = mpsc::channel();
        let worker = thread::Builder::new()
            .name(format!("beckon-display-{id}"))
            .spawn(move || {
                let runtime = match Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = status_sender.send(WorkerStatus::Failed(format!(
                            "initialize plugin worker runtime: {error}"
                        )));
                        return;
                    }
                };
                runtime.block_on(supervise(plugin, receiver, status_sender));
            })
            .with_context(|| format!("start display plugin worker {id}"))?;

        Ok(Self {
            id,
            updates: Some(updates),
            statuses,
            worker: Some(worker),
            last_plan: None,
            next_sequence: 1,
        })
    }

    fn queue(&mut self, plan: &RenderPlan) -> Result<()> {
        if self.last_plan.as_ref() == Some(plan) {
            return Ok(());
        }

        let sequence = self.next_sequence;
        let bytes = DisplayMessage::render(sequence, plan).to_ndjson()?;
        let updates = self
            .updates
            .as_ref()
            .ok_or_else(|| anyhow!("display plugin worker is shutting down"))?;
        if updates.is_closed() {
            bail!("display plugin worker stopped unexpectedly");
        }
        updates.send_replace(Some(QueuedRender {
            sequence,
            bytes: bytes.into(),
        }));
        self.last_plan = Some(plan.clone());
        self.next_sequence = self.next_sequence.saturating_add(1);
        Ok(())
    }
}

impl DisplaySink for ProcessDisplay {
    fn id(&self) -> &str {
        &self.id
    }

    fn publish(&mut self, plan: &RenderPlan) -> Result<PublishOutcome> {
        self.queue(plan)?;

        let mut latest = None;
        for status in self.statuses.try_iter() {
            latest = Some(status);
        }
        match latest {
            Some(WorkerStatus::Delivered) => Ok(PublishOutcome::Updated),
            Some(WorkerStatus::Failed(message)) => Err(anyhow!(message)),
            None => Ok(PublishOutcome::Unchanged),
        }
    }
}

impl Drop for ProcessDisplay {
    fn drop(&mut self) {
        self.updates.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

async fn supervise(
    plugin: ProcessOutputPlugin,
    mut updates: watch::Receiver<Option<QueuedRender>>,
    statuses: Sender<WorkerStatus>,
) {
    loop {
        let initial = loop {
            if let Some(render) = updates.borrow_and_update().clone() {
                break render;
            }
            if updates.changed().await.is_err() {
                return;
            }
        };

        match run_session(&plugin, &mut updates, &statuses, initial).await {
            Ok(()) => return,
            Err(error) => {
                let _ = statuses.send(WorkerStatus::Failed(format!("{error:#}")));
            }
        }

        tokio::select! {
            _ = sleep(RESTART_DELAY) => {}
            changed = updates.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

async fn run_session(
    plugin: &ProcessOutputPlugin,
    updates: &mut watch::Receiver<Option<QueuedRender>>,
    statuses: &Sender<WorkerStatus>,
    initial: QueuedRender,
) -> Result<()> {
    let executable = &plugin.command[0];
    let mut command = Command::new(executable);
    command
        .args(&plugin.command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn executable {executable:?}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("plugin child has no stdin"))?;

    let hello = DisplayMessage::hello(&plugin.id).to_ndjson()?;
    if let Err(error) = write_message(&mut stdin, &hello).await {
        stop_child(&mut child).await;
        return Err(error.context("write protocol hello"));
    }
    if let Err(error) = deliver(&mut stdin, &initial, statuses).await {
        stop_child(&mut child).await;
        return Err(error);
    }

    let mut exit_poll = interval(EXIT_POLL_INTERVAL);
    exit_poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            changed = updates.changed() => {
                if changed.is_err() {
                    drop(stdin);
                    stop_child_gracefully(&mut child).await;
                    return Ok(());
                }
                let render = updates.borrow_and_update().clone();
                if let Some(render) = render
                    && let Err(error) = deliver(&mut stdin, &render, statuses).await
                {
                    stop_child(&mut child).await;
                    return Err(error);
                }
            }
            _ = exit_poll.tick() => {
                if let Some(status) = child.try_wait().context("poll plugin process")? {
                    bail!("plugin process exited with {status}");
                }
            }
        }
    }
}

async fn deliver(
    stdin: &mut ChildStdin,
    render: &QueuedRender,
    statuses: &Sender<WorkerStatus>,
) -> Result<()> {
    write_message(stdin, &render.bytes)
        .await
        .with_context(|| format!("write render sequence {}", render.sequence))?;
    let _ = statuses.send(WorkerStatus::Delivered);
    Ok(())
}

async fn write_message(stdin: &mut ChildStdin, bytes: &[u8]) -> Result<()> {
    timeout(WRITE_TIMEOUT, stdin.write_all(bytes))
        .await
        .context("plugin stdin write timed out")??;
    Ok(())
}

async fn stop_child(child: &mut Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

async fn stop_child_gracefully(child: &mut Child) {
    if !matches!(timeout(SHUTDOWN_GRACE, child.wait()).await, Ok(Ok(_))) {
        stop_child(child).await;
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Instant};

    use super::*;
    use crate::render::DisplayConfig;

    fn plan() -> RenderPlan {
        crate::render::render(&DisplayConfig::default(), &[]).unwrap()
    }

    #[test]
    fn reports_missing_plugin_executable_without_blocking_publish() {
        let mut display = ProcessDisplay::new(ProcessOutputPlugin {
            id: "missing".into(),
            command: vec!["/path/that/does/not/exist/beckon-plugin".into()],
        })
        .unwrap();

        let started = Instant::now();
        assert_eq!(display.publish(&plan()).unwrap(), PublishOutcome::Unchanged);
        assert!(started.elapsed() < Duration::from_millis(100));

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match display.publish(&plan()) {
                Err(error) => {
                    assert!(error.to_string().contains("spawn executable"), "{error:#}");
                    break;
                }
                Ok(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                other => panic!("plugin failure was not reported: {other:?}"),
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn writes_hello_then_render_to_a_long_running_plugin() {
        let output = std::env::temp_dir().join(format!(
            "beckon-display-plugin-{}-{}.ndjson",
            std::process::id(),
            thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_file(&output);
        let mut display = ProcessDisplay::new(ProcessOutputPlugin {
            id: "capture".into(),
            command: vec!["tee".into(), output.display().to_string()],
        })
        .unwrap();

        assert_eq!(display.publish(&plan()).unwrap(), PublishOutcome::Unchanged);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(display.publish(&plan()), Ok(PublishOutcome::Updated)) {
                break;
            }
            assert!(Instant::now() < deadline, "plugin never accepted render");
            thread::sleep(Duration::from_millis(10));
        }

        let lines = loop {
            if let Ok(lines) = fs::read_to_string(&output)
                && lines.lines().count() == 2
            {
                break lines;
            }
            assert!(Instant::now() < deadline, "plugin did not consume render");
            thread::sleep(Duration::from_millis(10));
        };
        drop(display);

        let messages = lines
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["type"], "hello");
        assert_eq!(messages[0]["plugin_id"], "capture");
        assert_eq!(messages[1]["type"], "render");
        assert_eq!(messages[1]["sequence"], 1);
        assert_eq!(messages[1]["plan"]["keys"].as_array().unwrap().len(), 10);
        fs::remove_file(output).unwrap();
    }
}
