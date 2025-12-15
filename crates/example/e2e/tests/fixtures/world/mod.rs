mod action_steps;
mod check_steps;

use cucumber::World;
use thirtyfour::prelude::*;

pub const HOST: &str = "http://host.docker.internal:8080";
pub const SELENIUM_URL: &str = "http://localhost:4444";

#[derive(World)]
#[world(init = Self::new)]
pub struct AppWorld {
    pub driver: WebDriver,
}

impl std::fmt::Debug for AppWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppWorld")
            .field("driver", &"WebDriver")
            .finish()
    }
}

impl AppWorld {
    async fn new() -> Result<Self, anyhow::Error> {
        let driver = build_driver().await?;
        Ok(Self { driver })
    }
}

async fn build_driver() -> Result<WebDriver, anyhow::Error> {
    let mut caps = DesiredCapabilities::chrome();
    caps.add_arg("--disable-gpu")?;
    caps.add_arg("--no-sandbox")?;
    caps.add_arg("--disable-dev-shm-usage")?;

    // Connect to Selenium standalone container
    // Docker Selenium handles Chrome + ChromeDriver automatically
    let driver = WebDriver::new(SELENIUM_URL, caps).await?;

    Ok(driver)
}
