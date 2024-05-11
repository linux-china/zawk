use fake::{Fake};
use fake::faker::internet::raw::{FreeEmail, IPv4};
use fake::faker::name::raw::*;
use fake::faker::phone_number::raw::{CellNumber, PhoneNumber};
use fake::locales::*;

pub fn fake(name: &str, locale: &str) -> String {
    let locale = &locale.to_uppercase();
    return match name {
        "name" => {
            if locale == "ZH_CN" {
                Name(ZH_CN).fake()
            } else {
                Name(EN).fake()
            }
        }
        "phonenumber" | "phone" => {
            if locale == "ZH_CN" {
                PhoneNumber(ZH_CN).fake()
            } else {
                PhoneNumber(EN).fake()
            }
        }
        "cellnumber" | "cell" => {
            if locale == "ZH_CN" || locale == "CN" {
                CellNumber(ZH_CN).fake()
            } else {
                CellNumber(EN).fake()
            }
        }
        "email" => {
            FreeEmail(EN).fake()
        }
        "ip" | "ipv4" => {
            IPv4(EN).fake()
        }
        _ => {
            "".to_string()
        }
    };
}


#[cfg(test)]
mod tests {
    use fake::{Fake};
    use fake::faker::name::raw::*;
    use fake::locales::*;
    use super::*;

    #[test]
    fn test_fake_name() {
        println!("{}", fake("phone", "ZH_CN"));
    }

    #[test]
    fn test_name() {
        let name: String = Name(ZH_CN).fake();
        println!("name {:?}", name);
    }
}