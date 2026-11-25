use crate::config::{AppConfig, validate_name};
use crate::i18n::{I18n, Msg};
use crate::identity;

pub fn run(config: &AppConfig, i18n: &I18n, name: Option<&str>) {
    match name {
        None => match config.get_default().ok().flatten() {
            Some(current) => {
                let acc_dir = config.account_path(&current);
                let label = match identity::read_cache(&acc_dir).and_then(|c| c.email) {
                    Some(email) => format!("{} <{}>", current, email),
                    None => current,
                };
                i18n.print(Msg::DefaultCurrent(label));
            }
            None => i18n.print(Msg::DefaultStandard),
        },
        Some("default") => {
            config.clear_default().expect("Failed to update config");
            i18n.print(Msg::ResetDone);
        }
        Some(name) => {
            if !validate_name(name) {
                i18n.print(Msg::NameInvalid);
                std::process::exit(1);
            }
            // Existence check and write happen together under the routing lock,
            // so a concurrent `remove` of this account cannot slip between them
            // and leave the default pointing at an account that is gone.
            match config.set_default_checked(name) {
                Ok(true) => i18n.print(Msg::DefaultSet(name.to_string())),
                Ok(false) => {
                    i18n.print(Msg::DefaultNotFound(name.to_string()));
                    super::list::run(config, i18n);
                    std::process::exit(1);
                }
                Err(_) => panic!("Failed to update config"),
            }
        }
    }
}
