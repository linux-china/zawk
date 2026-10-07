use log::*;

/// Target prefix of the `log_*()` functions called from AWK scripts.
const SCRIPT_TARGET: &str = "zawk";

/// Default filters: dependencies only report warnings (noisy ones only errors), while logs from AWK scripts
/// (target `zawk`) keep the debug level. `RUST_LOG` directives are applied on top of them,
/// e.g. `RUST_LOG=zawk=warn` hides debug/info logs from scripts, `RUST_LOG=reqwest=debug` debugs HTTP calls.
const DEFAULT_FILTERS: &str = "warn,zawk=debug,\
    cranelift_codegen=error,cranelift_jit=error,\
    reqwest=error,hyper=error,hyper_util=error,hyper_rustls=error,rustls=error,\
    tokio_postgres=error,paho_mqtt=error,paho_mqtt_c=error";

/// Initialize logger, called from `main` after `.env` is loaded so `RUST_LOG` in `.env` is respected.
pub fn init() {
    let mut builder = env_logger::Builder::new();
    builder.parse_filters(DEFAULT_FILTERS);
    if let Ok(filters) = std::env::var("RUST_LOG") {
        builder.parse_filters(&filters);
    }
    if let Ok(write_style) = std::env::var("RUST_LOG_STYLE") {
        builder.parse_write_style(&write_style);
    }
    let _ = builder.target(env_logger::Target::Stderr).try_init();
}

/// Log target for AWK scripts: `zawk`, or `zawk:<FILENAME>` when an input file is being processed.
fn script_target(file_name: &str) -> String {
    if file_name.is_empty() {
        SCRIPT_TARGET.to_owned()
    } else {
        format!("{}:{}", SCRIPT_TARGET, file_name)
    }
}

pub fn log_debug(file_name: &str, text: &str) {
    debug!(target: &script_target(file_name), "{}", text);
}

pub fn log_info(file_name: &str, text: &str) {
    info!(target: &script_target(file_name), "{}", text);
}

pub fn log_warn(file_name: &str, text: &str) {
    warn!(target: &script_target(file_name), "{}", text);
}

pub fn log_error(file_name: &str, text: &str) {
    error!(target: &script_target(file_name), "{}", text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug() {
        log_debug("", "Hello");
    }

    #[test]
    fn test_script_target() {
        assert_eq!(script_target(""), "zawk");
        assert_eq!(script_target("demo.csv"), "zawk:demo.csv");
    }
}
