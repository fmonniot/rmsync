use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use clap::{App, AppSettings, Arg, SubCommand};

const VERSION: &'static str = env!("CARGO_PKG_VERSION");
const AUTHORS: &'static str = env!("CARGO_PKG_AUTHORS");

#[tokio::main]
async fn main() {
    env_logger::init();

    println!("1. parse cli arguments (clap ?)");
    let matches = App::new("rmsync")
        .version(VERSION)
        .author(AUTHORS)
        .about("Synchronise various Internet sources to the reMarkable Cloud")
        .setting(AppSettings::ArgRequiredElseHelp)
        .arg(Arg::with_name("config").help("Path to the configuration file"))
        .subcommand(
            SubCommand::with_name("ffnet")
                .about("FanFiction.net related features")
                .arg(
                    Arg::with_name("story_id")
                        .required(true)
                        .help("The story id as found in the url"),
                )
                .arg(Arg::with_name("chapter_num").required(false).help(
                    "An optional chapter number. If none given, the entire story will be used",
                )),
        )
        .get_matches();

    println!("2. load configuration for rmcloud (and create on demand if needed)");
    let mut cfg = match Config::read(matches.value_of("config").map(|p| p.into())) {
        Ok(c) => c,
        Err(e) => {
            println!("{}", e);
            return;
        }
    };

    println!("3. create rmcloud client");
    let mut rm_cloud = rmcloud::Client::from_tokens(&cfg.device_token(), cfg.user_token());
    // TODO Should be smarter here, and only refresh when we get an unauthorized error
    rm_cloud.renew_token().await.unwrap();

    if let Some(matches) = matches.subcommand_matches("ffnet") {
        let story_id = matches.value_of("story_id").unwrap();
        let story_id = match fanfictionnet::StoryId::from_str(story_id) {
            Some(sid) => sid,
            None => {
                println!("The given story id is invalid. You can find the story id in the url of a story. For example:
                https://www.fanfiction.net/s/4985743/38/The-Path-of-a-Jedi
                                             ^^^^^^^");
                return;
            }
        };

        let chapter_num = if let Some(n) = matches.value_of("chapter_num") {
            match fanfictionnet::ChapterNum::from_str(n) {
                Some(c) => Some(c),
                None => {
                    println!("The given chapter number is invalid. You can find the chapter in the url of a story. For example:
                    https://www.fanfiction.net/s/4985743/38/The-Path-of-a-Jedi
                                                         ^^");
                    return;
                }
            }
        } else {
            None
        };

        println!("sid: {:?}, chapter: {:?}", story_id, chapter_num);
        println!("4. call recipes::upload_ffnet_chapter");
        match chapter_num {
            Some(chapter) => {
                recipes::upload_ffnet_chapter(&rm_cloud, story_id, chapter)
                    .await
                    .unwrap();
            }
            None => {
                recipes::upload_ffnet_story(&rm_cloud, story_id)
                    .await
                    .unwrap();
            }
        }
    }

    let o = rm_cloud
        .user_token()
        .as_ref()
        .map(|t| t.as_str().to_string());
    cfg.set_user_token(o);

    match cfg.save() {
        Ok(()) => (),
        Err(err) => println!("Couldn't save configuration: {:?}", err),
    }
}

struct Config {
    file: ConfigFile,
    path: PathBuf,
}

impl Config {
    fn set_user_token(&mut self, user_token: Option<String>) {
        self.file.user_token = user_token;
    }

    fn device_token(&self) -> &str {
        &self.file.device_token
    }

    fn user_token(&self) -> Option<&String> {
        self.file.user_token.as_ref()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ConfigFile {
    device_token: String,
    user_token: Option<String>,
}

impl Config {
    pub fn read(path: Option<PathBuf>) -> Result<Config, String> {
        let config_path = path
            .or_else(|| {
                dirs::config_dir().map(|mut std| {
                    std.push("rmsync/configuration.json");
                    std
                })
            })
            .ok_or_else(|| {
                "No configuration file have been found. Please provide one with --config"
                    .to_string()
            })?;

        println!("config_path: {:?}", config_path);

        // Try to read the file, creating it if it doesn't exists
        let f = std::fs::read_to_string(&config_path);

        let file = match f {
            Ok(content) => {
                let c = serde_json::from_str(&content)
                    .map_err(|e| format!("Can't read the configuration: {:?}", e))?;

                c
            }
            Err(error) => match error.kind() {
                std::io::ErrorKind::NotFound => {
                    // TODO Finish this case. Currently all I got is a device code (not token)
                    let device_code = Config::ask_for_device_code().expect("");

                    //let c = rmcloud::Client::from_tokens(None, None);
                    //c.register(device_code)

                    let cfg = ConfigFile {
                        device_token: device_code,
                        user_token: None,
                    };

                    let parent = config_path.parent().unwrap();
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("Couldn't create config directory: {:?}", e))?;

                    std::fs::File::create(&config_path)
                        .map_err(|e| format!("Problem creating the config file: {:?}", e))
                        .and_then(|fc| {
                            serde_json::to_writer(fc, &cfg)
                                .map_err(|e| format!("Problem writing the config file: {:?}", e))
                        })?;

                    cfg
                }
                _ => {
                    panic!("Problem opening the file: {:?}", error)
                }
            },
        };

        Ok(Config {
            file,
            path: config_path,
        })
    }

    fn ask_for_device_code() -> std::io::Result<String> {
        println!("Please go on https://my.remarkable.com/connect/desktop, connect and type the code here:");
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer)?;

        Ok(buffer.trim().to_string())
    }

    fn save(&self) -> Result<(), String> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map_err(|e| format!("{:?}", e))?;
        serde_json::to_writer_pretty(file, &self.file).map_err(|e| format!("{:?}", e))?;

        Ok(())
    }
}
