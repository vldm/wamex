use thirtyfour::prelude::*;

use super::find::*;

pub async fn goto_path(driver: &WebDriver, path: &str) -> Result<(), WebDriverError> {
    let url = format!("{}{}", super::world::HOST, path);
    driver.goto(&url).await
}

pub async fn enter_text_into_input(driver: &WebDriver, text: &str) -> Result<(), WebDriverError> {
    let input = find_input_by_id(driver, "url").await?;
    input.clear().await?;
    input.send_keys(text).await?;
    Ok(())
}

pub async fn submit_form(driver: &WebDriver) -> Result<(), WebDriverError> {
    let button = find_button_by_type(driver).await?;
    button.click().await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    Ok(())
}
