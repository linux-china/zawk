use log::*;

#[ctor::ctor]
fn init() {
    env_logger::builder().filter_level(LevelFilter::Debug).init();
}

pub fn log_debug(text: &str) {
    debug!("{}", text);
}

pub fn log_info(text: &str) {
    info!("{}", text);
}

pub fn log_warn(text: &str) {
    warn!("{}", text);
}

pub fn log_error(text: &str) {
    error!("{}", text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug() {
        log_debug("Hello");
    }
}