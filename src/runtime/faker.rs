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
        "id" => generate_chinese_id_number(),
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

// A sample of real county-level administrative division codes (GB/T 2260).
const ID_REGION_CODES: [u32; 32] = [
    110101, 110105, 110108, 120101, 130102, 140105, 150102, 210102, 220102, 230102, 310101,
    310115, 320102, 330106, 340102, 350102, 360102, 370102, 410105, 420106, 430104, 440106,
    440305, 450103, 460106, 500103, 510104, 520102, 530102, 610113, 620102, 650102,
];
// ISO 7064 MOD 11-2 weights for the first 17 digits.
const ID_WEIGHTS: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
const ID_CHECK_CHARS: [char; 11] = ['1', '0', 'X', '9', '8', '7', '6', '5', '4', '3', '2'];

/// Chinese resident identity card number (GB 11643-1999):
/// 6-digit region code + 8-digit birth date + 3-digit sequence + check digit.
fn generate_chinese_id_number() -> String {
    use chrono::{Datelike, NaiveDate};
    use rand::RngExt;
    let rng = &mut rand::rng();
    let region = ID_REGION_CODES.choose(rng).unwrap();
    let start = NaiveDate::from_ymd_opt(1950, 1, 1).unwrap().num_days_from_ce();
    let end = NaiveDate::from_ymd_opt(2010, 12, 31).unwrap().num_days_from_ce();
    let birth = NaiveDate::from_num_days_from_ce_opt(rng.random_range(start..=end)).unwrap();
    let sequence: u32 = rng.random_range(1..=999);
    let body = format!("{region}{}{sequence:03}", birth.format("%Y%m%d"));
    format!("{body}{}", id_check_char(&body))
}

fn id_check_char(body: &str) -> char {
    let sum: u32 = body
        .bytes()
        .zip(ID_WEIGHTS)
        .map(|(b, w)| (b - b'0') as u32 * w)
        .sum();
    ID_CHECK_CHARS[(sum % 11) as usize]
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
        // Known valid number from GB 11643-1999.
        assert_eq!(id_check_char("11010519491231002"), 'X');
        for _ in 0..1000 {
            let id = fake("id", "EN");
            assert_eq!(id.len(), 18, "bad length: {id:?}");
            assert!(id[..17].bytes().all(|b| b.is_ascii_digit()), "bad digits: {id:?}");
            assert!(chrono::NaiveDate::parse_from_str(&id[6..14], "%Y%m%d").is_ok(), "bad date: {id:?}");
            assert_eq!(id.chars().last().unwrap(), id_check_char(&id[..17]), "bad check: {id:?}");
        }
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
