use beckon::config;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "beckon-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(directory.join("config.d")).unwrap();
        let fixture = Self(directory);
        let mut root: toml::Value =
            toml::from_str(include_str!("../templates/config.toml")).unwrap();
        root["bindings"] =
            toml::Value::try_from((1..=10).map(|i| format!("key{i}")).collect::<Vec<_>>()).unwrap();
        root["sources"] = toml::from_str::<toml::Value>(
            "sources=[{id='rex',list=['false'],focus=[],activate=[],refresh_ms=1000}]",
        )
        .unwrap()["sources"]
            .clone();
        fs::write(
            fixture.0.join("config.toml"),
            toml::to_string(&root).unwrap(),
        )
        .unwrap();
        fixture
    }
    fn load(&self) -> anyhow::Result<config::Config> {
        config::load_at(&self.0.join("config.toml"))
    }
    fn device(&self, name: &str, text: &str) {
        fs::write(self.0.join("config.d").join(format!("{name}.toml")), text).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn three_devices_load_and_removing_one_removes_its_bindings() {
    let fixture = Fixture::new();
    fixture.device(
        "creator-micro",
        include_str!("../templates/creator-micro.toml"),
    );
    fixture.device("macbook", include_str!("../templates/macbook.toml"));
    fixture.device("glove80", include_str!("../templates/glove80.toml"));
    assert_eq!(fixture.load().unwrap().devices.len(), 3);
    fs::remove_file(fixture.0.join("config.d/macbook.toml")).unwrap();
    let config = fixture.load().unwrap();
    assert_eq!(config.devices.len(), 2);
    assert!(config.devices.iter().all(|device| device.id != "macbook"));
}

#[test]
fn offline_validation_finds_typos_missing_references_and_alias_collisions() {
    let fixture = Fixture::new();
    fixture.device(
        "macbook",
        "adapter='hotkeys'\n[keys.key1]\nmembers=['macbook:F1']\nbinding='ke1'",
    );
    let error = format!("{:#}", fixture.load().unwrap_err());
    assert!(
        error.contains("did you mean") && error.contains("macbook.toml"),
        "{error}"
    );
    fixture.device("macbook","adapter='hotkeys'\n[keys.a]\nmembers=['macbook:Ctrl+D']\n[keys.b]\nmembers=['macbook:Control+KeyD']");
    assert!(format!("{:#}", fixture.load().unwrap_err()).contains("claimed"));
}

#[test]
fn symlinked_device_files_load_and_mutable_settings_cannot_add_actions() {
    let fixture = Fixture::new();
    std::os::unix::fs::symlink(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates/macbook.toml"),
        fixture.0.join("config.d/macbook.toml"),
    )
    .unwrap();
    assert_eq!(fixture.load().unwrap().devices.len(), 1);
    fs::write(
        fixture.0.join("settings.toml"),
        "[actions.injected]\nkind='chord'",
    )
    .unwrap();
    assert!(fixture.load().is_err());
}

#[test]
fn theme_mutations_validate_then_persist_only_mutable_settings() {
    let fixture = Fixture::new();
    let original = fs::read(fixture.0.join("config.toml")).unwrap();
    let mut config = fixture.load().unwrap();
    assert!(config.set_theme("typo").is_err());
    assert!(!fixture.0.join("settings.toml").exists());
    config.set_theme("status").unwrap();
    assert_eq!(fixture.load().unwrap().display.theme, "status");
    assert_eq!(fs::read(fixture.0.join("config.toml")).unwrap(), original);
}

#[test]
fn synthesized_outputs_must_not_feed_back_into_registered_inputs() {
    let fixture = Fixture::new();
    fixture.device("macbook","adapter='hotkeys'\n[keys.a]\nmembers=['macbook:Alt+Control+D']\naction='same'\n[actions.same]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='hold'");
    assert!(format!("{:#}", fixture.load().unwrap_err()).contains("feedback loop"));
}
