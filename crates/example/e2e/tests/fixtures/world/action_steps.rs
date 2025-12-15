use cucumber::{given, when};
use crate::fixtures::{action, world::AppWorld};

#[given("the app is running")]
async fn app_is_running(world: &mut AppWorld) {
    action::goto_path(&world.driver, "/").await.unwrap();
}

#[when(regex = r#"I enter "([^"]*)" into the input field"#)]
async fn enter_text(world: &mut AppWorld, text: String) {
    action::enter_text_into_input(&world.driver, &text).await.unwrap();
}

#[when("I submit the form")]
async fn submit_form(world: &mut AppWorld) {
    action::submit_form(&world.driver).await.unwrap();
}
