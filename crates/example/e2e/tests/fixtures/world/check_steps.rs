use cucumber::then;

use crate::fixtures::{check, world::AppWorld};

#[then(regex = r#"the result should contain "([^"]*)""#)]
async fn result_contains(world: &mut AppWorld, expected: String) {
    check::result_contains(&world.driver, &expected)
        .await
        .unwrap();
}

#[then(regex = r#"the result should be "([^"]*)""#)]
async fn result_equals(world: &mut AppWorld, expected: String) {
    check::result_equals(&world.driver, &expected)
        .await
        .unwrap();
}

#[then(regex = r#"the WASM module "([^"]*)" should be loaded"#)]
async fn wasm_module_loaded(world: &mut AppWorld, module_name: String) {
    check::wasm_module_loaded(&world.driver, &module_name)
        .await
        .unwrap();
}
