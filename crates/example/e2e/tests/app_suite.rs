use std::fs::read_dir;
use std::ffi::OsStr;
use cucumber::World;

mod fixtures;

use fixtures::world::AppWorld;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for entry in read_dir("./features")? {
        let path = entry?.path();
        if path.extension() == Some(OsStr::new("feature")) {
            AppWorld::cucumber()
                .fail_on_skipped()
                .run_and_exit(path)
                .await;
        }
    }
    Ok(())
}
