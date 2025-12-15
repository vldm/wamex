@split_modules
Feature: Check that split modules load correctly

  Scenario: Static string module loads correctly
    Given the app is running
    When I enter "static" into the input field
    And I submit the form
    Then the result should contain "SUPER STATIC SHARED STRING"
    And the WASM module "static_str" should be loaded

  Scenario: String build module loads correctly
    Given the app is running
    When I enter "string" into the input field
    And I submit the form
    Then the result should contain "OTHER STATIC STRING"
    And the result should contain "small addition"
    And the WASM module "string_from_static" should be loaded

  Scenario: String build with shared const module loads correctly
    Given the app is running
    When I enter "string_shared" into the input field
    And I submit the form
    Then the result should contain "SUPER STATIC SHARED STRING"
    And the result should contain "hi"
    And the WASM module "string_build_with_shared_const" should be loaded

  Scenario: Async string module loads correctly
    Given the app is running
    When I enter "async" into the input field
    And I submit the form
    Then the result should contain "ASYNC STRING"
    And the WASM module "async_string" should be loaded

  Scenario: Dynamic functions module loads correctly
    Given the app is running
    When I enter "dyn" into the input field
    And I submit the form
    Then the result should contain "DYN"
    And the result should contain "FROM INNER"
    And the WASM module "multiple_dyn_fns" should be loaded

  Scenario: Dependent dynamic module loads correctly
    Given the app is running
    When I enter "dep_dyn" into the input field
    And I submit the form
    Then the result should contain "DYN FNS"
    And the result should contain "FROM INNER"
    And the WASM module "dep_dyn" should be loaded

  Scenario: Lifetime module loads correctly
    Given the app is running
    When I enter "lieftime" into the input field
    And I submit the form
    Then the result should contain "test"
    And the WASM module "use_lifetime" should be loaded

  Scenario: Fallback returns input unchanged
    Given the app is running
    When I enter "unknown_input" into the input field
    And I submit the form
    Then the result should be "unknown_input"
