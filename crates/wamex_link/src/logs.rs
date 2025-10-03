#[cfg(feature = "debug")]
#[macro_export]
macro_rules! debug {

    ($($arg:tt)+) => {
        log::debug!($($arg)+)
    };

}
#[cfg(not(feature = "debug"))]
#[macro_export]
macro_rules! debug {
    ($($arg:tt)+) => {};
}
