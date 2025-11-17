#[cfg(feature = "debug")]
#[macro_export]
macro_rules! debug {

    ($($arg:tt)+) => {
        log::debug!($($arg)+)
    };

}

#[cfg(feature = "debug")]
#[macro_export]
macro_rules! trace {

    ($($arg:tt)+) => {
        log::trace!($($arg)+)
    };

}

#[cfg(feature = "debug")]
#[macro_export]
macro_rules! warn {

    ($($arg:tt)+) => {
        log::warn!($($arg)+)
    };

}

#[cfg(feature = "debug")]
#[macro_export]
macro_rules! error {

    ($($arg:tt)+) => {
        log::error!($($arg)+)
    };

}
#[cfg(not(feature = "debug"))]
#[macro_export]
macro_rules! debug {
    ($($arg:tt)+) => {};
}

#[cfg(not(feature = "debug"))]
#[macro_export]
macro_rules! warn {
    ($($arg:tt)+) => {};
}

#[cfg(not(feature = "debug"))]
#[macro_export]
macro_rules! error {
    ($($arg:tt)+) => {};
}

#[cfg(not(feature = "debug"))]
#[macro_export]
macro_rules! trace {
    ($($arg:tt)+) => {};
}
