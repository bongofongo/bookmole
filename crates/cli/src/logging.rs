use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Default is `error`, not `warn`: at `warn` each degradation would print twice, once as an event
/// and once as the command's own `warning:` line.
fn directive(verbose: u8, quiet: bool) -> String {
    if quiet {
        return "off".to_string();
    }
    match verbose {
        0 => "bookmole=error".to_string(),
        1 => "bookmole=info".to_string(),
        2 => "bookmole=debug".to_string(),
        _ => "bookmole=trace,bookmole_cli=trace".to_string(),
    }
}

/// Installs the subscriber on stderr so stdout stays pipeable.
pub fn init(verbose: u8, quiet: bool) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(directive(verbose, quiet)));

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal());

    if verbose <= 1 {
        builder.without_time().with_target(false).init();
    } else {
        builder.init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_beats_verbosity() {
        assert_eq!(directive(3, true), "off");
    }

    #[test]
    fn default_is_quiet_enough_not_to_double_report() {
        assert_eq!(directive(0, false), "bookmole=error");
    }

    #[test]
    fn verbosity_climbs_and_then_saturates() {
        assert_eq!(directive(1, false), "bookmole=info");
        assert_eq!(directive(2, false), "bookmole=debug");
        assert_eq!(directive(3, false), directive(9, false));
    }
}
