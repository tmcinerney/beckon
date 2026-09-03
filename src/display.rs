//! Hardware-neutral display fan-out and output adapters.

mod process;

use anyhow::Result;

use crate::{
    config::{OutputAdapter, OutputConfig},
    hid::{self, StatusWriter, UsbStatusWriter},
    render::RenderPlan,
};

use process::ProcessDisplay;

/// Result of offering the current render plan to one display integration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The adapter sent a new state to its device or service.
    Updated,
    /// Nothing was sent because the state is unchanged or retry is deferred.
    Unchanged,
    /// The optional target is not currently attached or available.
    Unavailable,
}

/// One independently managed consumer of Beckon's hardware-neutral state.
pub trait DisplaySink {
    fn id(&self) -> &str;
    fn publish(&mut self, plan: &RenderPlan) -> Result<PublishOutcome>;
}

/// The Glove80's optional wired USB status display.
pub struct Glove80UsbDisplay<W = UsbStatusWriter> {
    sink: hid::RenderSink<W>,
}

impl Default for Glove80UsbDisplay<UsbStatusWriter> {
    fn default() -> Self {
        Self::new(UsbStatusWriter)
    }
}

impl<W> Glove80UsbDisplay<W>
where
    W: StatusWriter,
{
    pub fn new(writer: W) -> Self {
        Self {
            sink: hid::RenderSink::new(writer),
        }
    }
}

impl<W> DisplaySink for Glove80UsbDisplay<W>
where
    W: StatusWriter,
{
    fn id(&self) -> &str {
        OutputAdapter::Glove80Usb.name()
    }

    fn publish(&mut self, plan: &RenderPlan) -> Result<PublishOutcome> {
        match self.sink.publish(plan) {
            Ok(true) => Ok(PublishOutcome::Updated),
            Ok(false) => Ok(PublishOutcome::Unchanged),
            Err(error) if hid::is_status_endpoint_unavailable(&error) => {
                Ok(PublishOutcome::Unavailable)
            }
            Err(error) => Err(error),
        }
    }
}

/// A newly reportable failure from one display integration.
#[derive(Debug, PartialEq, Eq)]
pub struct DisplayFailure {
    pub adapter: String,
    pub message: String,
}

struct ManagedSink {
    sink: Box<dyn DisplaySink>,
    last_error: Option<String>,
}

/// Fans one render plan out to zero or more independent display integrations.
///
/// Adapter absence and failures never block input or pane navigation. Errors
/// are de-duplicated per adapter and retried by the adapter on later ticks.
#[derive(Default)]
pub struct DisplaySet {
    sinks: Vec<ManagedSink>,
}

impl DisplaySet {
    pub fn from_config(config: &OutputConfig) -> Result<Self> {
        config.validate()?;
        let mut sinks = config
            .adapters
            .iter()
            .map(|adapter| match adapter {
                OutputAdapter::Glove80Usb => {
                    Box::new(Glove80UsbDisplay::default()) as Box<dyn DisplaySink>
                }
            })
            .map(|sink| ManagedSink {
                sink,
                last_error: None,
            })
            .collect::<Vec<_>>();
        for plugin in &config.plugins {
            sinks.push(ManagedSink {
                sink: Box::new(ProcessDisplay::new(plugin.clone())?),
                last_error: None,
            });
        }
        Ok(Self { sinks })
    }

    #[cfg(test)]
    fn new(sinks: Vec<Box<dyn DisplaySink>>) -> Self {
        Self {
            sinks: sinks
                .into_iter()
                .map(|sink| ManagedSink {
                    sink,
                    last_error: None,
                })
                .collect(),
        }
    }

    pub fn publish(&mut self, plan: &RenderPlan) -> Vec<DisplayFailure> {
        let mut failures = Vec::new();
        for managed in &mut self.sinks {
            match managed.sink.publish(plan) {
                Ok(PublishOutcome::Updated) => managed.last_error = None,
                Ok(PublishOutcome::Unchanged | PublishOutcome::Unavailable) => {}
                Err(error) => {
                    let message = format!("{error:#}");
                    if managed.last_error.as_deref() != Some(message.as_str()) {
                        failures.push(DisplayFailure {
                            adapter: managed.sink.id().to_string(),
                            message: message.clone(),
                        });
                        managed.last_error = Some(message);
                    }
                }
            }
        }
        failures
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    use super::*;
    use crate::render::DisplayConfig;

    struct FakeSink {
        id: &'static str,
        outcomes: VecDeque<Result<PublishOutcome>>,
        calls: Rc<RefCell<usize>>,
    }

    impl DisplaySink for FakeSink {
        fn id(&self) -> &str {
            self.id
        }

        fn publish(&mut self, _plan: &RenderPlan) -> Result<PublishOutcome> {
            *self.calls.borrow_mut() += 1;
            self.outcomes
                .pop_front()
                .unwrap_or(Ok(PublishOutcome::Unchanged))
        }
    }

    fn plan() -> RenderPlan {
        crate::render::render(&DisplayConfig::default(), &[]).unwrap()
    }

    #[test]
    fn empty_display_set_is_valid() {
        assert!(DisplaySet::default().publish(&plan()).is_empty());
    }

    #[test]
    fn unavailable_sink_is_an_expected_condition() {
        let calls = Rc::new(RefCell::new(0));
        let sink = FakeSink {
            id: "optional",
            outcomes: [Ok(PublishOutcome::Unavailable)].into(),
            calls: calls.clone(),
        };
        let mut displays = DisplaySet::new(vec![Box::new(sink)]);

        assert!(displays.publish(&plan()).is_empty());
        assert_eq!(*calls.borrow(), 1);
    }

    struct UnavailableWriter;

    impl StatusWriter for UnavailableWriter {
        fn write_snapshot(&mut self, _snapshot: hid::StatusSnapshot) -> Result<()> {
            Err(hid::StatusEndpointUnavailable.into())
        }
    }

    #[test]
    fn glove80_adapter_maps_a_missing_endpoint_to_unavailable() {
        let mut display = Glove80UsbDisplay::new(UnavailableWriter);

        assert_eq!(
            display.publish(&plan()).unwrap(),
            PublishOutcome::Unavailable
        );
    }

    #[test]
    fn isolates_and_deduplicates_sink_failures() {
        let failed_calls = Rc::new(RefCell::new(0));
        let healthy_calls = Rc::new(RefCell::new(0));
        let failed = FakeSink {
            id: "failed",
            outcomes: [
                Err(anyhow::anyhow!("transport failed")),
                Err(anyhow::anyhow!("transport failed")),
                Ok(PublishOutcome::Updated),
                Err(anyhow::anyhow!("transport failed")),
            ]
            .into(),
            calls: failed_calls.clone(),
        };
        let healthy = FakeSink {
            id: "healthy",
            outcomes: [
                Ok(PublishOutcome::Updated),
                Ok(PublishOutcome::Updated),
                Ok(PublishOutcome::Updated),
                Ok(PublishOutcome::Updated),
            ]
            .into(),
            calls: healthy_calls.clone(),
        };
        let mut displays = DisplaySet::new(vec![Box::new(failed), Box::new(healthy)]);

        assert_eq!(displays.publish(&plan()).len(), 1);
        assert!(displays.publish(&plan()).is_empty());
        assert!(displays.publish(&plan()).is_empty());
        assert_eq!(displays.publish(&plan()).len(), 1);
        assert_eq!(*failed_calls.borrow(), 4);
        assert_eq!(*healthy_calls.borrow(), 4);
    }

    #[test]
    fn unchanged_does_not_claim_recovery_from_an_error() {
        let calls = Rc::new(RefCell::new(0));
        let sink = FakeSink {
            id: "backing-off",
            outcomes: [
                Err(anyhow::anyhow!("transport failed")),
                Ok(PublishOutcome::Unchanged),
                Err(anyhow::anyhow!("transport failed")),
            ]
            .into(),
            calls,
        };
        let mut displays = DisplaySet::new(vec![Box::new(sink)]);

        assert_eq!(displays.publish(&plan()).len(), 1);
        assert!(displays.publish(&plan()).is_empty());
        assert!(displays.publish(&plan()).is_empty());
    }
}
