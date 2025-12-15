use thirtyfour::prelude::*;

pub async fn find_input_by_id(driver: &WebDriver, id: &str) -> Result<WebElement, WebDriverError> {
    driver.find(By::Id(id)).await
}

pub async fn find_button_by_type(driver: &WebDriver) -> Result<WebElement, WebDriverError> {
    driver.find(By::Css("input[type='submit']")).await
}

pub async fn find_result_textarea(driver: &WebDriver) -> Result<WebElement, WebDriverError> {
    driver.find(By::Id("result")).await
}
