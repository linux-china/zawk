//! AWK functions backed by the `TypeSafe` System One API, see [`super::jev`].
//!
//! * `jev(record, instructions)`: 1 when the probability of yes is above the threshold, else 0
//! * `jev_prob(record, instructions)`: the probability of yes, from 0 to 1
//! * `jev_choice(record, instructions, options)`: the picked option
//! * `jev_score(record, instructions, levels)`: the weighted position on the levels
//!
//! The compiler passes every array or text argument through `to_json`, so the functions here
//! receive JSON text: a string is the text to evaluate, an object or array is structured data.
//!
//! The options (levels) array maps a name to its description; an entry whose key and value are
//! the same, or whose key is an index (`split` arrays), is a plain option without description.
//! A plain text options argument is a comma separated list.
use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex, OnceLock};

use serde_json::Value;

use crate::runtime::jev::{Question, Questions, State, TypeSafeClient, TypeSafeError, JEV_THRESHOLD};

const QUESTION_ID: &str = "q";

static CLIENT: OnceLock<Result<TypeSafeClient, String>> = OnceLock::new();

/// Answers by request: the same record asked the same question is evaluated once.
static CACHE: LazyLock<Mutex<HashMap<String, Answer>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
enum Answer {
    Probability(f64),
    Choice(String),
    Score(f64),
}

fn client() -> Result<&'static TypeSafeClient, String> {
    CLIENT
        .get_or_init(|| TypeSafeClient::from_env().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(Clone::clone)
}

/// The state of a request from the JSON text of the record argument.
fn state_of(record: &str) -> State {
    match serde_json::from_str::<Value>(record) {
        Ok(Value::String(text)) => State::Text(text),
        Ok(Value::Null) => State::Text(String::new()),
        Ok(value @ (Value::Object(_) | Value::Array(_))) => State::Json(value),
        Ok(value) => State::Text(value.to_string()),
        Err(_) => State::Text(record.to_owned()),
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Options as (name, description) pairs from the JSON text of the options argument.
fn options_of(options: &str) -> Vec<(String, Option<String>)> {
    let plain = |name: String| (name.trim().to_owned(), None);
    match serde_json::from_str::<Value>(options) {
        Ok(Value::Array(items)) => items.iter().map(|item| plain(value_text(item))).collect(),
        Ok(Value::Object(entries)) => entries
            .into_iter()
            .map(|(key, value)| {
                let description = value_text(&value);
                if key == description || description.is_empty() {
                    (key, None)
                } else {
                    (key, Some(description))
                }
            })
            .collect(),
        Ok(Value::String(text)) => text.split(',').map(|name| plain(name.to_owned())).collect(),
        Ok(value) => vec![plain(value_text(&value))],
        Err(_) => options.split(',').map(|name| plain(name.to_owned())).collect(),
    }
    .into_iter()
    .filter(|(name, _)| !name.is_empty())
    .collect()
}

fn ask(record: &str, question: Question) -> Result<Answer, String> {
    let state = state_of(record);
    let mut questions = Questions::new();
    questions.insert(QUESTION_ID.to_owned(), question);
    let cache_key = crate::runtime::jev::request_body(&state, &questions);
    if let Some(answer) = CACHE.lock().unwrap().get(&cache_key) {
        return Ok(answer.clone());
    }
    let kind = questions[QUESTION_ID].clone();
    let evaluation = client()?
        .evaluate(&state, questions)
        .map_err(|e: TypeSafeError| e.to_string())?;
    let answer = match kind {
        Question::Noul { .. } => Answer::Probability(evaluation.yes_no(QUESTION_ID).map_err(|e| e.to_string())?),
        Question::Choice { .. } => Answer::Choice(evaluation.choice(QUESTION_ID).map_err(|e| e.to_string())?.choice),
        Question::Score { .. } => Answer::Score(evaluation.score(QUESTION_ID).map_err(|e| e.to_string())?.score),
    };
    CACHE.lock().unwrap().insert(cache_key, answer.clone());
    Ok(answer)
}

fn report(function: &str, error: &str) {
    eprintln!("zawk: {function}: {error}");
}

/// The threshold of `jev`: the `jev_threshold` variable when it is set (above 0), then the
/// `JEV_THRESHOLD` environment variable, then 0.5.
fn threshold(variable: f64) -> f64 {
    if variable > 0.0 {
        return variable;
    }
    std::env::var("JEV_THRESHOLD")
        .ok()
        .and_then(|text| text.trim().parse::<f64>().ok())
        .filter(|value| *value > 0.0)
        .unwrap_or(JEV_THRESHOLD as f64)
}

/// `jev_prob(record, instructions)`: the probability of yes, -1 when the request fails.
pub fn jev_prob(record: &str, instructions: &str) -> f64 {
    match ask(record, Question::yes_no(instructions)) {
        Ok(Answer::Probability(yes)) => yes,
        Ok(_) => -1.0,
        Err(error) => {
            report("jev_prob", &error);
            -1.0
        }
    }
}

/// `jev(record, instructions)`: 1 when the probability of yes is above the threshold, else 0.
pub fn jev(record: &str, instructions: &str, threshold_variable: f64) -> i64 {
    match ask(record, Question::yes_no(instructions)) {
        Ok(Answer::Probability(yes)) => (yes > threshold(threshold_variable)) as i64,
        Ok(_) => 0,
        Err(error) => {
            report("jev", &error);
            0
        }
    }
}

/// `jev_choice(record, instructions, options)`: the picked option, empty when the request fails.
pub fn jev_choice(record: &str, instructions: &str, options: &str) -> String {
    let criteria: BTreeMap<String, Option<String>> = options_of(options).into_iter().collect();
    if criteria.is_empty() {
        report("jev_choice", "no options given");
        return String::new();
    }
    let question = Question::Choice { instructions: instructions.to_owned(), criteria };
    match ask(record, question) {
        Ok(Answer::Choice(choice)) => choice,
        Ok(_) => String::new(),
        Err(error) => {
            report("jev_choice", &error);
            String::new()
        }
    }
}

/// `jev_score(record, instructions, levels)`: the probability-weighted position on the levels,
/// -1 when the request fails.
pub fn jev_score(record: &str, instructions: &str, levels: &str) -> f64 {
    let criteria: Vec<String> = options_of(levels)
        .into_iter()
        .map(|(name, description)| match description {
            Some(description) => format!("{name}: {description}"),
            None => name,
        })
        .collect();
    if criteria.len() < 2 {
        report("jev_score", "at least two levels are required");
        return -1.0;
    }
    let question = Question::Score { instructions: instructions.to_owned(), criteria };
    match ask(record, question) {
        Ok(Answer::Score(score)) => score,
        Ok(_) => -1.0,
        Err(error) => {
            report("jev_score", &error);
            -1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_of() {
        assert_eq!(state_of("\"hello\""), State::Text("hello".to_owned()));
        assert_eq!(state_of("hello"), State::Text("hello".to_owned()));
        assert!(matches!(state_of("{\"name\":\"Jacky\"}"), State::Json(_)));
    }

    #[test]
    fn test_options_of() {
        assert_eq!(
            options_of("[\"billing\",\"technical\"]"),
            vec![("billing".to_owned(), None), ("technical".to_owned(), None)]
        );
        assert_eq!(
            options_of("{\"billing\":\"billing\",\"security\":\"account access\"}"),
            vec![("billing".to_owned(), None), ("security".to_owned(), Some("account access".to_owned()))]
        );
        assert_eq!(
            options_of("\"budget, premium\""),
            vec![("budget".to_owned(), None), ("premium".to_owned(), None)]
        );
    }

    #[test]
    fn test_threshold() {
        assert_eq!(threshold(0.8), 0.8);
    }

    #[test]
    fn test_jev_functions() {
        dotenv::dotenv().ok();
        if std::env::var("TYPESAFE_API_KEY").is_err() {
            return;
        }
        let record = "{\"name\":\"Hans Müller\",\"city\":\"Berlin\"}";
        let yes = jev_prob(record, "the name is European");
        println!("jev_prob: {yes}");
        assert!((0.0..=1.0).contains(&yes));
        assert_eq!(jev(record, "the name is European", 0.0), 1);
        let text = "\"I was charged twice for my subscription this month!\"";
        let team = jev_choice(text, "which team should handle this?", "[\"billing\",\"technical\",\"security\",\"sales\"]");
        println!("jev_choice: {team}");
        assert_eq!(team, "billing");
        let product = "\"Hand-stitched Italian leather handbag with 18k gold hardware\"";
        let score = jev_score(product, "how luxurious is this product?", "[\"budget\",\"mid-range\",\"premium\",\"luxury\"]");
        println!("jev_score: {score}");
        assert!(score >= 0.0);
    }
}
