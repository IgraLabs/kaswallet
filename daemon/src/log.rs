use log::LevelFilter;
use log4rs::append::console::ConsoleAppender;
use log4rs::append::rolling_file::policy::compound::roll::fixed_window::FixedWindowRoller;
use log4rs::append::rolling_file::policy::compound::trigger::size::SizeTrigger;
use log4rs::append::rolling_file::policy::compound::CompoundPolicy;
use log4rs::append::rolling_file::RollingFileAppender;
use log4rs::config::{Appender, Root};
use log4rs::encode::pattern::PatternEncoder;
use log4rs::filter::threshold::ThresholdFilter;
use log4rs::Config;
use std::error::Error;
use std::path::Path;

pub fn init_log(logs_path: String, log_level: LevelFilter) -> Result<(), Box<dyn Error>> {
    let general_log_path = Path::new(&logs_path).join("kaswallet.log");
    let err_log_path = Path::new(&logs_path).join("kaswallet.err.log");

    let encoder = Box::new(PatternEncoder::new(
        "{d(%Y-%m-%dT%H:%M:%S)(utc)} [{l}] {m}{n}",
    ));

    let stdout = ConsoleAppender::builder().encoder(encoder.clone()).build();

    let fixed_window_roller_general = Box::new(FixedWindowRoller::builder().build(
        &format!("{}{}.gz", general_log_path.clone().display(), "{}"),
        10,
    )?);
    let fixed_window_roller_err = Box::new(FixedWindowRoller::builder().build(
        &format!("{}{}.gz", err_log_path.clone().display(), "{}"),
        10,
    )?);
    let trigger = Box::new(SizeTrigger::new(10_000));
    let rolling_policy_general = Box::new(CompoundPolicy::new(
        trigger.clone(),
        fixed_window_roller_general,
    ));
    let rolling_policy_err = Box::new(CompoundPolicy::new(trigger, fixed_window_roller_err));

    let file = RollingFileAppender::builder()
        .encoder(encoder.clone())
        .build(general_log_path, rolling_policy_general)?;
    let file_err = RollingFileAppender::builder().encoder(encoder).build(
        Path::new(&logs_path).join("kaswallet.err.log"),
        rolling_policy_err,
    )?;

    let config = Config::builder()
        .appender(Appender::builder().build("stdout", Box::new(stdout)))
        .appender(Appender::builder().build("file", Box::new(file)))
        .appender(
            Appender::builder()
                .filter(Box::new(ThresholdFilter::new(LevelFilter::Warn)))
                .build("file_err", Box::new(file_err)),
        )
        .build(
            Root::builder()
                .appender("stdout")
                .appender("file")
                .appender("file_err")
                .build(log_level),
        )?;

    log4rs::init_config(config)?;

    Ok(())
}
