use fake::{Fake};
use fake::faker::address::raw::{PostCode, ZipCode};
use fake::faker::automotive::raw::LicencePlate;
use fake::faker::company::raw::CompanyName;
use fake::faker::creditcard::en::CreditCardNumber;
use fake::faker::internet::raw::{FreeEmail, IPv4};
use fake::faker::name::raw::*;
use fake::faker::phone_number::raw::{CellNumber, PhoneNumber};
use fake::locales::*;
use rand::seq::IndexedRandom;

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
        "plate" => {
            if is_chinese(locale) {
                generate_chinese_plate_number()
            } else {
                LicencePlate(FR_FR).fake()
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


const PROVINCE_SHOT_NAMES: [char; 31] = ['京', '津', '晋', '冀', '蒙', '辽', '吉', '黑', '沪', '苏', '浙', '皖', '闽', '赣',
    '鲁', '豫', '鄂', '湘', '粤', '桂', '琼', '渝', '川', '贵', '云', '藏', '陕', '甘', '青', '宁', '新'];
const LICENSE_CHARS: [char; 23] = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'N', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y'];

fn generate_chinese_plate_number() -> String {
    use rand::Rng;
    let rng = &mut rand::rng();
    let province: &char = PROVINCE_SHOT_NAMES.choose(rng).unwrap();
    let alphabet: &char = LICENSE_CHARS.choose(rng).unwrap();
    format!("{}{}{}{}{}{}{}", province, alphabet,
            rng.random_range(0..=9),
            rng.random_range(0..=9),
            rng.random_range(0..=9),
            rng.random_range(0..=9),
            rng.random_range(0..=9))
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

    #[test]
    fn test_plate() {
        println!("{}", fake("plate", "ZH_CN"));
    }
}