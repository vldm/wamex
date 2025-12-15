use thirtyfour::prelude::*;

use super::find::*;

pub async fn result_contains(driver: &WebDriver, expected: &str) -> Result<(), anyhow::Error> {
    let result = find_result_textarea(driver).await?;
    let text = result.text().await?;

    if !text.contains(expected) {
        anyhow::bail!(
            "Expected result to contain '{}', but got: '{}'",
            expected,
            text
        );
    }

    Ok(())
}

pub async fn result_equals(driver: &WebDriver, expected: &str) -> Result<(), anyhow::Error> {
    let result = find_result_textarea(driver).await?;
    let text = result.text().await?;

    if text != expected {
        anyhow::bail!(
            "Expected result to equal '{}', but got: '{}'",
            expected,
            text
        );
    }

    Ok(())
}

pub async fn wasm_module_loaded(
    driver: &WebDriver,
    module_name: &str,
) -> Result<(), anyhow::Error> {
    // Check browser network activity via JavaScript to verify WASM file was loaded
    let script = format!(
        r#"
        return performance.getEntriesByType('resource')
            .filter(entry => entry.name.includes('.wasm'))
            .filter(entry => entry.name.includes('{}'))
            .length > 0;
        "#,
        module_name
    );

    let result = driver.execute(&script, vec![]).await?;

    // Convert ScriptRet to serde_json::Value and check if it's true
    let json_value = result.json();
    let is_loaded = json_value.as_bool().unwrap_or(false);

    if is_loaded {
        Ok(())
    } else {
        // Get all WASM files loaded for debugging
        let all_wasm_script = r#"
            return performance.getEntriesByType('resource')
                .filter(entry => entry.name.includes('.wasm'))
                .map(entry => entry.name);
        "#;
        let all_wasm = driver.execute(all_wasm_script, vec![]).await?;
        let all_wasm_json = all_wasm.json();

        anyhow::bail!(
            "Expected WASM module '{}' to be loaded, but it was not found. Loaded WASM files: {:?}",
            module_name,
            all_wasm_json
        );
    }
}
