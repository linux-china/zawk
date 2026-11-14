use fake::Fake;
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
    match name {
        "name" => {
            if is_chinese(locale) {
                Name(ZH_CN).fake()
            } else {
                Name(EN).fake()
            }
        }
        // Only Chinese resident identity card numbers are generated, whatever the locale.
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
        "email" => FreeEmail(EN).fake(),
        "ip" | "ipv4" => IPv4(EN).fake(),
        "creditcard" => CreditCardNumber().fake(),
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
        "wechat" => generate_wechat_id(),
        _ => "".to_string(),
    }
}

fn is_chinese(locale: &str) -> bool {
    locale == "ZH_CN" || locale == "CN" || locale == "ZH"
}

const PROVINCE_SHOT_NAMES: [char; 31] = [
    '京', '津', '晋', '冀', '蒙', '辽', '吉', '黑', '沪', '苏', '浙', '皖', '闽', '赣', '鲁', '豫',
    '鄂', '湘', '粤', '桂', '琼', '渝', '川', '贵', '云', '藏', '陕', '甘', '青', '宁', '新',
];
// Letters on Chinese plates (GA 36): A-Z except `I` and `O`.
const LICENSE_CHARS: [char; 24] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'N', 'P', 'Q', 'R', 'S', 'T', 'U',
    'V', 'W', 'X', 'Y', 'Z',
];

fn generate_chinese_plate_number() -> String {
    use rand::RngExt;
    let rng = &mut rand::rng();
    let province: &char = PROVINCE_SHOT_NAMES.choose(rng).unwrap();
    let alphabet: &char = LICENSE_CHARS.choose(rng).unwrap();
    format!(
        "{}{}{}{}{}{}{}",
        province,
        alphabet,
        rng.random_range(0..=9),
        rng.random_range(0..=9),
        rng.random_range(0..=9),
        rng.random_range(0..=9),
        rng.random_range(0..=9)
    )
}

const WECHAT_MIN_LEN: usize = 6;
const WECHAT_MAX_LEN: usize = 20;

// 以字母开头和 6-20 位数字、字母、下划线、减号的组合
fn generate_wechat_id() -> String {
    use rand::RngExt;
    let rng = &mut rand::rng();
    let name: String = Name(EN).fake();
    let mut id: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if !id.starts_with(|c: char| c.is_ascii_alphabetic()) {
        let letter = (b'a' + rng.random_range(0..26u8)) as char;
        id.insert(0, letter);
    }
    id.truncate(WECHAT_MAX_LEN);
    if id.len() < WECHAT_MIN_LEN {
        let target = rng.random_range(WECHAT_MIN_LEN..=WECHAT_MIN_LEN + 4);
        while id.len() < target {
            id.push((b'0' + rng.random_range(0..10u8)) as char);
        }
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use fake::Fake;

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
        for _ in 0..5000 {
            let id = fake("wechat", "ZH_CN");
            assert!(
                (WECHAT_MIN_LEN..=WECHAT_MAX_LEN).contains(&id.len()),
                "bad length: {id:?}"
            );
            let mut chars = id.chars();
            assert!(chars.next().unwrap().is_ascii_alphabetic(), "bad first char: {id:?}");
            assert!(
                chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "bad chars: {id:?}"
            );
        }
    }

    #[test]
    fn test_plate() {
        println!("{}", fake("plate", "ZH_CN"));
    }
}
