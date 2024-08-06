use std::fmt::{Display, Formatter};
use std::io::Read;
use std::path::{Path, PathBuf};

use bincode::config::standard;
use bincode::error::DecodeError;
use bincode::decode_from_std_read;
use cu29::copperlist::CopperList;
use cu29_intern_strs::read_interned_strings;
use cu29_log::{rebuild_logline, CuLogEntry};
use cu29_traits::{CuError, CuResult, UnifiedLogType};

use clap::{Parser, Subcommand, ValueEnum};
use cu29_traits::CopperListPayload;
use cu29_unifiedlog::{UnifiedLogger, UnifiedLoggerBuilder, UnifiedLoggerIOReader};

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum ExportFormat {
    Json,
    Csv,
}

impl Display for ExportFormat {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportFormat::Json => write!(f, "json"),
            ExportFormat::Csv => write!(f, "csv"),
        }
    }
}

/// This is a generator for a main function to build a log extractor.
#[derive(Parser)]
#[command(author, version, about)]
pub struct LogReaderCli {
    pub unifiedlog: PathBuf,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Extract logs
    ExtractLog { log_index: PathBuf },
    /// Extract copperlists
    ExtractCopperlist {
        #[arg(short, long, default_value_t = ExportFormat::Json)]
        export_format: ExportFormat,
    },
}

/// This is a generator for a main function to build a log extractor.
/// It depends on the specific type of the CopperList payload that is determined at compile time from the configuration.
pub fn run_cli<P>() -> CuResult<()>
where
    P: CopperListPayload,
{
    let args = LogReaderCli::parse();
    let unifiedlog = args.unifiedlog;

    let UnifiedLogger::Read(dl) = UnifiedLoggerBuilder::new()
        .file_path(&unifiedlog)
        .build()
        .expect("Failed to create logger")
    else {
        panic!("Failed to create logger");
    };

    match args.command {
        Command::ExtractLog { log_index } => {
            let reader = UnifiedLoggerIOReader::new(dl, UnifiedLogType::StructuredLogLine);
            textlog_dump(reader, &log_index)?;
        }
        Command::ExtractCopperlist { export_format } => {
            println!("Extracting copperlists with format: {}", export_format);
            let mut reader = UnifiedLoggerIOReader::new(dl, UnifiedLogType::CopperList);
            let iter = copperlists_dump::<P>(&mut reader);
            for entry in iter {
                println!("{:#?}", entry);
            }
        }
    }

    Ok(())
}

/// Extracts the copper lists from a binary representation.
/// P is the Payload determined by the configuration of the application.
pub fn copperlists_dump<P: CopperListPayload>(
    mut src: impl Read,
) -> impl Iterator<Item = CopperList<P>> {
    std::iter::from_fn(move || {
        let entry = decode_from_std_read::<CopperList<P>, _, _>(&mut src, standard());
        match entry {
            Ok(entry) => Some(entry),
            Err(e) => match e {
                DecodeError::UnexpectedEnd { .. } => return None,
                DecodeError::Io { inner, additional } => {
                    if inner.kind() == std::io::ErrorKind::UnexpectedEof {
                        return None;
                    } else {
                        println!("Error {:?} additional:{}", inner, additional);
                        return None;
                    }
                }
                _ => {
                    println!("Error {:?}", e);
                    return None;
                }
            },
        }
    })
}

/// Full dump of the copper structured log from its binary representation.
/// This rebuilds a textual log.
/// src: the source of the log data
/// index: the path to the index file (containing the interned strings constructed at build time)
pub fn textlog_dump(mut src: impl Read, index: &Path) -> CuResult<()> {
    let all_strings = read_interned_strings(index)?;
    loop {
        let entry = decode_from_std_read::<CuLogEntry, _, _>(&mut src, standard());

        match entry {
            Err(DecodeError::UnexpectedEnd { .. }) => return Ok(()),
            Err(DecodeError::Io { inner, additional }) => {
                if inner.kind() == std::io::ErrorKind::UnexpectedEof {
                    return Ok(());
                } else {
                    println!("Error {:?} additional:{}", inner, additional);
                    return Err(CuError::new_with_cause("Error reading log", inner));
                }
            }
            Err(e) => {
                println!("Error {:?}", e);
                return Err(CuError::new_with_cause("Error reading log", e));
            }
            Ok(entry) => {
                if entry.msg_index == 0 {
                    break;
                }

                let result = rebuild_logline(&all_strings, &entry);
                if result.is_err() {
                    println!("Failed to rebuild log line: {:?}", result);
                    continue;
                }
                println!("Culog: [{}] {}", entry.time, result.unwrap());
            }
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bincode::encode_into_slice;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};
    use fs_extra::dir::{copy, CopyOptions};
    use tempfile::{tempdir, TempDir};

    use cu29_clock::RobotClock;
    use cu29_log::value::Value;
    use cu29_log_runtime::log;
    use cu29_log_runtime::LoggerRuntime;
    use cu29_traits::UnifiedLogType;
    use cu29_unifiedlog::{
        stream_write, UnifiedLogger, UnifiedLoggerBuilder, UnifiedLoggerIOReader,
    };

    use super::*;

    fn copy_stringindex_to_temp(tmpdir:&TempDir) -> PathBuf {
        // for some reason using the index in real only locks it and generates a change in the file.
        let temp_path = tmpdir.path();

        let mut copy_options = CopyOptions::new();
        copy_options.copy_inside = true;

        copy("test/cu29_log_index", &temp_path, &copy_options).unwrap();
        temp_path.join("cu29_log_index")
    }

    #[test]
    fn test_extract_low_level_cu29_log() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = copy_stringindex_to_temp(&temp_dir);
        let entry = CuLogEntry::new(3);
        let bytes = bincode::encode_to_vec(&entry, standard()).unwrap();
        let reader = Cursor::new(bytes.as_slice());
        textlog_dump(reader, temp_path.as_path()).unwrap();
    }


    #[test]
    fn end_to_end_datalogger_and_structlog_test() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir
            .path()
            .join("end_to_end_datalogger_and_structlog_test.copper");
        {
            // Write a couple log entries
            let UnifiedLogger::Write(logger) = UnifiedLoggerBuilder::new()
                .write(true)
                .create(true)
                .file_path(&path)
                .preallocated_size(100000)
                .build()
                .expect("Failed to create logger")
            else {
                panic!("Failed to create logger")
            };
            let data_logger = Arc::new(Mutex::new(logger));
            let stream = stream_write(data_logger.clone(), UnifiedLogType::StructuredLogLine, 1024);
            let rt = LoggerRuntime::init(RobotClock::default(), stream, None);

            let mut entry = CuLogEntry::new(4); // this is a "Just a String {}" log line
            entry.add_param(0, Value::String("Parameter for the log line".into()));
            log(entry).expect("Failed to log");
            let mut entry = CuLogEntry::new(2); // this is a "Just a String {}" log line
            entry.add_param(0, Value::String("Parameter for the log line".into()));
            log(entry).expect("Failed to log");

            // everything is dropped here
            drop(rt);
        }
        // Read back the log
        let UnifiedLogger::Read(logger) = UnifiedLoggerBuilder::new()
            .file_path(&path)
            .build()
            .expect("Failed to create logger")
        else {
            panic!("Failed to create logger")
        };
        let reader = UnifiedLoggerIOReader::new(logger, UnifiedLogType::StructuredLogLine);
        let temp_dir = TempDir::new().unwrap();
        textlog_dump(reader, Path::new(copy_stringindex_to_temp(&temp_dir).as_path())).expect("Failed to dump log");
    }

    // This is normally generated at compile time in CuPayload.
    type MyCuPayload = (u8, i32, f32);

    /// Checks if we can recover the copper lists from a binary representation.
    #[test]
    fn test_copperlists_dump() {
        let mut data = vec![0u8; 10000];
        let mypls: [MyCuPayload; 4] = [(1, 2, 3.0), (2, 3, 4.0), (3, 4, 5.0), (4, 5, 6.0)];

        let mut offset: usize = 0;
        for pl in mypls.iter() {
            let cl = CopperList::<MyCuPayload>::new(1, *pl);
            offset +=
                encode_into_slice(&cl, &mut data.as_mut_slice()[offset..], standard()).unwrap();
        }

        let reader = Cursor::new(data);

        let mut iter = copperlists_dump::<MyCuPayload>(reader);
        assert_eq!(iter.next().unwrap().payload, (1, 2, 3.0));
        assert_eq!(iter.next().unwrap().payload, (2, 3, 4.0));
        assert_eq!(iter.next().unwrap().payload, (3, 4, 5.0));
        assert_eq!(iter.next().unwrap().payload, (4, 5, 6.0));
    }
}
