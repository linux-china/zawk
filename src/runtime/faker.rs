use fake::{Fake};
use fake::faker::address::raw::{PostCode, ZipCode};
use fake::faker::company::raw::CompanyName;
use fake::faker::creditcard::en::CreditCardNumber;
use fake::faker::internet::raw::{FreeEmail, IPv4};
use fake::faker::name::raw::*;
use fake::faker::phone_number::raw::{CellNumber, PhoneNumber};
use fake::locales::*;

pub fn fake(name: &str, locale: &str) -> String {
    let locale = &locale.to_uppercase();
    return match name {
        "name" => {
            if is_chinese(locale) {
                Name(ZH_CN).fake()
            } else {
                Name(EN).fake()
            }
        }
        "id" => {
            identitycard::random::generate_identitycard("".to_owned(), "".to_owned()).to_string()
        }
        "phonenumber" | "phone" => {
            if is_chinese(locale) {
                PhoneNumber(ZH_CN).fake()
            } else {
                PhoneNumber(EN).fake()
            }
        }
        "cellnumber" | "cell" => {
            if is_chinese(locale) {
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
        "creditcard" => {
            CreditCardNumber().fake()
        }
        "company" => {
            if is_chinese(locale) {
                CompanyName(ZH_CN).fake()
            } else {
                CompanyName(EN).fake()
            }
        }
        "zipcode" => {
            if is_chinese(locale) {
                ZipCode(ZH_CN).fake()
            } else {
                ZipCode(EN).fake()
            }
        }
        "postcode" => {
            if is_chinese(locale) {
                PostCode(ZH_CN).fake()
            } else {
                PostCode(EN).fake()
            }
        }
        "wechat" => {
            // 以字母开头和 6-20 位数字、字母、下划线、减号的组合
            let name: String = Name(EN).fake();
            name.replace(" ", "-").to_string()
        }
        _ => {
            "".to_string()
        }
    };
}

fn is_chinese(locale: &str) -> bool {
    locale == "ZH_CN" || locale == "CN" || locale == "ZH"
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

    #[test]
    fn test_creditcard() {
        println!("{}", fake("creditcard", "EN"));
    }

    #[test]
    fn test_company() {
        println!("{}", fake("company", "ZH_CN"));
    }

    #[test]
    fn test_zipcode() {
        println!("{}", fake("postcode", "ZH_CN"));
    }

    #[test]
    fn test_id() {
        println!("{}", fake("id", "ZH_CN"));
    }

    #[test]
    fn test_wechat() {
        println!("{}", fake("wechat", "ZH_CN"));
    }
}