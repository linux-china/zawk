use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use std::{
    collections::BTreeMap,
    env,
    fmt::{Display, Formatter, Result as FmtResult},
    fs,
    path::Path,
    thread,
    time::Duration,
};

use reqwest::{
    StatusCode,
    blocking::{Client, Response},
};

const MODEL: &str = "jev-latest";
pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
pub const JEV_THRESHOLD: f32 = 0.5;

const MAX_ATTEMPTS: u32 = 3;
const BACKOFF_BASE_MS: u64 = 500;
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Debug, Error)]
pub enum TypeSafeError {
    /// The API key is missing or empty.
    #[error("no TypeSafe API key: {0}")]
    MissingKey(String),
    /// A key file could not be read.
    #[error("cannot read key file: {0}")]
    KeyFile(#[from] std::io::Error),
    /// Transport failure: connection, timeout or unreadable body.
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The API refused the request; carries the HTTP status and body message.
    #[error("typesafe API error {status}: {message}")]
    Api {
        /// HTTP status of the refusal.
        status: u16,
        /// Body message of the refusal.
        message: String,
    },
    /// A success body does not match the documented response shape.
    #[error("invalid response body: {0}")]
    InvalidResponse(String),
    /// An answer is missing or has a different shape than requested.
    #[error("unexpected answer for question {question:?}: {detail}")]
    UnexpectedAnswer {
        /// Id of the question whose answer is missing or misshapen.
        question: String,
        /// What was found instead.
        detail: String,
    },
}

/// Application state sent with a request.
///
/// The JSON wire shape stays an implementation detail: text travels as a
/// string, detailed state as an object with the running text under `"text"`
/// and the named facts under `"facts"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Free-form running text, e.g. a room description.
    Text(String),
    /// Running text plus named facts, e.g. inventory or flags.
    Detailed {
        /// The running text.
        text: String,
        /// Named facts keyed by name.
        facts: BTreeMap<String, String>,
    },
}

impl State {
    /// Plain-text state.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// Running text plus named facts given as name/description pairs.
    #[must_use]
    pub fn detailed(text: impl Into<String>, facts: &[(&str, &str)]) -> Self {
        Self::Detailed {
            text: text.into(),
            facts: facts
                .iter()
                .map(|&(name, fact)| (name.to_owned(), fact.to_owned()))
                .collect(),
        }
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Text(text) => Value::String(text.clone()),
            Self::Detailed { text, facts } => serde_json::json!({
                "text": text,
                "facts": facts,
            }),
        }
    }
}

impl From<&str> for State {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

impl From<String> for State {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&String> for State {
    fn from(text: &String) -> Self {
        Self::text(text.clone())
    }
}

/// Questions sent with one request, keyed by an id the caller owns.
pub type Questions = BTreeMap<String, Question>;

/// One typed judgment requested from the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Whether a condition holds; the answer is a probability of yes.
    Noul {
        /// What to judge, with backticked paths into the state.
        instructions: String,
        /// Truth conditions; absent means the instructions stand alone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Which of a defined set applies; the answer picks one option.
    Choice {
        /// What to decide between the options.
        instructions: String,
        /// Option name to description; a missing description serializes as `null`.
        criteria: BTreeMap<String, Option<String>>,
    },
    /// Degree along a described dimension; the answer is a weighted position.
    Score {
        /// What to place on the scale.
        instructions: String,
        /// Ordered levels, at least two, each describing a concrete situation.
        criteria: Vec<String>,
    },
}

impl Question {
    /// A [`Question::Noul`] question without explicit truth conditions.
    #[must_use]
    pub fn yes_no(instructions: impl Into<String>) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    /// A [`Question::Noul`] question that spells out what yes and no mean.
    ///
    /// Sends the same JSON as [`Question::yes_no`] plus a `criteria` object,
    /// so a caller that needs both texts no longer names the enum's fields.
    #[must_use]
    pub fn noul(
        instructions: impl Into<String>,
        is_true: impl Into<String>,
        is_false: impl Into<String>,
    ) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: Some(NoulCriteria {
                is_true: is_true.into(),
                is_false: is_false.into(),
            }),
        }
    }

    /// A [`Question::Choice`] question over option name/description pairs.
    #[must_use]
    pub fn choice(instructions: impl Into<String>, options: &[(&str, &str)]) -> Self {
        Self::Choice {
            instructions: instructions.into(),
            criteria: options
                .iter()
                .map(|&(name, description)| (name.to_owned(), Some(description.to_owned())))
                .collect(),
        }
    }

    /// A [`Question::Score`] question over ordered level descriptions.
    #[must_use]
    pub fn score(instructions: impl Into<String>, levels: &[&str]) -> Self {
        Self::Score {
            instructions: instructions.into(),
            criteria: levels.iter().map(|&level| level.to_owned()).collect(),
        }
    }
}

/// Truth conditions of a [`Question::Noul`] question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoulCriteria {
    /// When the answer counts as yes.
    #[serde(rename = "true")]
    pub is_true: String,
    /// When the answer counts as no.
    #[serde(rename = "false")]
    pub is_false: String,
}

/// One typed answer returned by the model.
///
/// Deserializes from either dialect: the hosted API tags each answer with a
/// `type` field, a self-hosted backend commonly omits it. The primitive is
/// read from whichever of `noul`, `choice` or `score` the answer carries, so
/// the tag is accepted when present and not required. A backend that reports
/// no distribution or no confidence yields an empty distribution and zero
/// confidence rather than an error.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Probability of yes.
    Noul {
        /// Probability of yes.
        noul: f64,
    },
    /// The picked option with its distribution.
    Choice {
        /// The picked option.
        choice: String,
        /// Probability per option.
        probabilities: BTreeMap<String, f64>,
        /// Concentration of the distribution.
        confidence: f64,
    },
    /// The weighted position with its distribution.
    Score {
        /// Probability-weighted position on the levels.
        score: f64,
        /// Level name to its description.
        legend: BTreeMap<String, String>,
        /// Probability per level.
        probabilities: BTreeMap<String, f64>,
        /// Concentration of the distribution.
        confidence: f64,
    },
}

/// Token usage of one evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct Usage {
    /// Tokens read.
    #[serde(default)]
    pub input_tokens: u64,
    /// Tokens written.
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Deserialize)]
struct RawAnswer {
    noul: Option<f64>,
    choice: Option<String>,
    score: Option<f64>,
    #[serde(default)]
    legend: BTreeMap<String, String>,
    #[serde(default)]
    probabilities: BTreeMap<String, f64>,
    #[serde(default)]
    confidence: f64,
}

impl<'de> Deserialize<'de> for Answer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let RawAnswer {
            noul,
            choice,
            score,
            legend,
            probabilities,
            confidence,
        } = RawAnswer::deserialize(deserializer)?;
        match (noul, choice, score) {
            (Some(noul), None, None) => Ok(Self::Noul { noul }),
            (None, Some(choice), None) => Ok(Self::Choice {
                choice,
                probabilities,
                confidence,
            }),
            (None, None, Some(score)) => Ok(Self::Score {
                score,
                legend,
                probabilities,
                confidence,
            }),
            _ => Err(serde::de::Error::custom(
                "an answer carries exactly one of noul, choice or score",
            )),
        }
    }
}

/// The model's answers to one request.
///
/// `model` names whichever backend answered, and is empty when the backend
/// reports none. `usage` counts zero tokens when the backend reports no usage,
/// as a self-hosted one commonly does. Fields a backend adds beyond these —
/// Laya's `routing` metadata, for one — are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Evaluation {
    /// Model that answered, empty when the backend names none.
    #[serde(default)]
    pub model: String,
    /// Answers keyed by the question ids of the request.
    pub answers: BTreeMap<String, Answer>,
    /// Token usage of the request, zero when the backend reports none.
    #[serde(default)]
    pub usage: Usage,
}

impl Evaluation {
    /// The [`Answer::Choice`] answered under `id`.
    ///
    /// Reads one answer of a multi-question request without matching on
    /// [`Answer`] at the call site.
    ///
    /// # Errors
    ///
    /// When no answer carries that id, or it is of another type.
    pub fn choice(&self, id: &str) -> Result<ChoiceAnswer, TypeSafeError> {
        match self.answers.get(id) {
            Some(Answer::Choice {
                choice,
                probabilities,
                confidence,
            }) => Ok(ChoiceAnswer {
                choice: choice.clone(),
                probabilities: probabilities.clone(),
                confidence: *confidence,
            }),
            other => Err(TypeSafeError::UnexpectedAnswer {
                question: id.to_owned(),
                detail: format!("expected a choice answer, found {other:?}"),
            }),
        }
    }

    /// The probability of yes answered under `id`.
    ///
    /// # Errors
    ///
    /// When no answer carries that id, or it is of another type.
    pub fn yes_no(&self, id: &str) -> Result<f64, TypeSafeError> {
        match self.answers.get(id) {
            Some(Answer::Noul { noul }) => Ok(*noul),
            other => Err(TypeSafeError::UnexpectedAnswer {
                question: id.to_owned(),
                detail: format!("expected a noul answer, found {other:?}"),
            }),
        }
    }

    /// The [`Answer::Score`] answered under `id`.
    ///
    /// # Errors
    ///
    /// When no answer carries that id, or it is of another type.
    pub fn score(&self, id: &str) -> Result<ScoreAnswer, TypeSafeError> {
        match self.answers.get(id) {
            Some(Answer::Score {
                score,
                legend,
                probabilities,
                confidence,
            }) => Ok(ScoreAnswer {
                score: *score,
                legend: legend.clone(),
                probabilities: probabilities.clone(),
                confidence: *confidence,
            }),
            other => Err(TypeSafeError::UnexpectedAnswer {
                question: id.to_owned(),
                detail: format!("expected a score answer, found {other:?}"),
            }),
        }
    }
}

/// The picked option of a [`Question::Choice`] question.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceAnswer {
    /// The picked option.
    pub choice: String,
    /// Probability per option.
    pub probabilities: BTreeMap<String, f64>,
    /// Concentration of the distribution.
    pub confidence: f64,
}

impl ChoiceAnswer {
    /// Picks an option by its probability instead of taking the likeliest.
    ///
    /// Walks the options in order, subtracting each probability from `roll`,
    /// and returns the one the roll lands in: an option the model gave a
    /// fifth of the mass to comes up for a fifth of the rolls. Lets a scene
    /// play differently from one run to the next without ever picking an
    /// option the model ruled out. `roll` is a value from 0 to 1, supplied by
    /// the caller so the draw stays reproducible; outside that range, and
    /// when no probabilities were returned, the answer's own
    /// [`choice`](Self::choice) comes back.
    #[must_use]
    pub fn sample(&self, roll: f64) -> &str {
        let mut remaining = roll;
        for (option, probability) in &self.probabilities {
            remaining -= *probability;
            if remaining < 0.0 {
                return option;
            }
        }
        &self.choice
    }
}

/// The weighted position of a [`Question::Score`] question.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreAnswer {
    /// Probability-weighted position on the levels.
    pub score: f64,
    /// Level name to its description.
    pub legend: BTreeMap<String, String>,
    /// Probability per level.
    pub probabilities: BTreeMap<String, f64>,
    /// Concentration of the distribution.
    pub confidence: f64,
}

impl Display for Answer {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Noul { noul } => write_noul(f, *noul),
            Self::Choice {
                choice,
                probabilities,
                confidence,
            } => write_choice(f, choice, probabilities, *confidence),
            Self::Score {
                score,
                legend,
                probabilities,
                confidence,
            } => write_score(f, *score, legend, probabilities, *confidence),
        }
    }
}

impl Display for ChoiceAnswer {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write_choice(f, &self.choice, &self.probabilities, self.confidence)
    }
}

impl Display for ScoreAnswer {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write_score(
            f,
            self.score,
            &self.legend,
            &self.probabilities,
            self.confidence,
        )
    }
}

impl Display for Evaluation {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        for (id, answer) in &self.answers {
            writeln!(f, "{id}: {answer}")?;
        }
        write!(
            f,
            "usage: {} tokens in, {} out ({})",
            self.usage.input_tokens, self.usage.output_tokens, self.model
        )
    }
}

fn percent(probability: f64) -> String {
    format!("{:.0}%", probability * 100.0)
}

/// Renders a yes probability as a verdict with its certainty.
///
/// Above one half the verdict is yes, below it no; the certainty stretches the
/// distance from the coin flip to the full range, so 0.15 reads as
/// `no, 70% sure` and 0.94 as `yes, 88% sure`. A probability outside 0 to 1
/// is clamped into it, so the certainty never exceeds 100%.
#[must_use]
pub fn verdict(yes: f64) -> String {
    let yes = yes.clamp(0.0, 1.0);
    let certainty = 2.0f64.mul_add(yes, -1.0).abs();
    if yes >= 0.5 {
        format!("yes, {} sure", percent(certainty))
    } else {
        format!("no, {} sure", percent(certainty))
    }
}

/// A yes/no judgment that may also come out undecided.
///
/// What [`verdict`] renders for a reader, this decides for a caller: a
/// consumer that falls back to non-AI behavior when the model is unsure
/// branches on this instead of comparing probabilities by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Yes, at least as certain as the threshold asked for.
    Yes,
    /// Too close to the coin flip to call either way.
    Unsure,
    /// No, at least as certain as the threshold asked for.
    No,
}

impl Verdict {
    /// Reads a yes probability as a verdict, undecided below `threshold`.
    ///
    /// The threshold is a certainty on the same stretched scale [`verdict`]
    /// prints, not a raw probability: it measures the distance from the coin
    /// flip, so `0.8` accepts a yes from 0.9 and a no from 0.1, and leaves
    /// everything between them [`Verdict::Unsure`]. A threshold of `0.0`
    /// never returns [`Verdict::Unsure`] and splits at one half, exactly as
    /// [`verdict`] does. Both arguments are clamped into 0 to 1, so a
    /// threshold above one decides nothing rather than everything.
    #[must_use]
    pub fn from_probability(yes: f64, threshold: f64) -> Self {
        let yes = yes.clamp(0.0, 1.0);
        let threshold = threshold.clamp(0.0, 1.0);
        let certainty = 2.0f64.mul_add(yes, -1.0).abs();
        if certainty < threshold {
            Self::Unsure
        } else if yes >= 0.5 {
            Self::Yes
        } else {
            Self::No
        }
    }
}

impl Display for Verdict {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.write_str(match self {
            Self::Yes => "yes",
            Self::Unsure => "unsure",
            Self::No => "no",
        })
    }
}

fn write_noul(f: &mut Formatter<'_>, noul: f64) -> FmtResult {
    f.write_str(&verdict(noul))
}

fn write_choice(
    f: &mut Formatter<'_>,
    choice: &str,
    probabilities: &BTreeMap<String, f64>,
    confidence: f64,
) -> FmtResult {
    let mut ranked: Vec<_> = probabilities.iter().collect();
    ranked.sort_by(|(_, a), (_, b)| b.total_cmp(a));
    write!(f, "{choice} (")?;
    for (index, (option, probability)) in ranked.into_iter().enumerate() {
        if index > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{option} {}", percent(*probability))?;
    }
    write!(f, "; confidence {})", percent(confidence))
}

fn write_score(
    f: &mut Formatter<'_>,
    score: f64,
    legend: &BTreeMap<String, String>,
    probabilities: &BTreeMap<String, f64>,
    confidence: f64,
) -> FmtResult {
    let mut levels: Vec<_> = legend.iter().collect();
    levels.sort_by(|(a, _), (b, _)| {
        a.parse::<u64>()
            .ok()
            .cmp(&b.parse::<u64>().ok())
            .then_with(|| a.cmp(b))
    });
    write!(f, "{score:.2} (")?;
    for (index, (key, name)) in levels.into_iter().enumerate() {
        if index > 0 {
            f.write_str(", ")?;
        }
        let probability = probabilities.get(key).copied().unwrap_or_default();
        write!(f, "{name} {}", percent(probability))?;
    }
    write!(f, "; confidence {})", percent(confidence))
}

/// Blocking client for the `TypeSafe` System One API.
#[derive(Debug, Clone)]
pub struct TypeSafeClient {
    key: Option<String>,
    endpoint: String,
    model: String,
    timeout: Duration,
    client: Client,
}

impl TypeSafeClient {
    /// Reads the key from the `TYPESAFE_API_KEY` environment variable.
    ///
    /// # Errors
    ///
    /// When the variable is missing or holds a blank key.
    #[must_use = "a client does nothing until it evaluates"]
    pub fn from_env() -> Result<Self, TypeSafeError> {
        Self::with_key(&key_from_env()?)
    }

    /// Reads the key as the first line of the file at `path`.
    ///
    /// Lets the owner supply a key without it ever appearing in a command line.
    /// Accepts any path-like value, including `&str` and `&Path`.
    ///
    /// # Errors
    ///
    /// When the file cannot be read or its first line is blank.
    #[must_use = "a client does nothing until it evaluates"]
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, TypeSafeError> {
        Self::with_key(&key_from_path(path)?)
    }

    /// Uses `key` as the API key.
    ///
    /// # Errors
    ///
    /// When the key is blank.
    #[must_use = "a client does nothing until it evaluates"]
    pub fn from_key(key: &str) -> Result<Self, TypeSafeError> {
        Self::with_key(key)
    }

    fn with_key(key: &str) -> Result<Self, TypeSafeError> {
        let key = checked_key(key)?;
        Ok(Self::build(Some(key))?.with_endpoint(ENDPOINT))
    }

    /// A client that sends no `Authorization` header at all.
    ///
    /// For a self-hosted System One backend on your own machine or network,
    /// which has no API key to send: give it the endpoint it listens on and
    /// the model name it answers under. The hosted API refuses an unauthorized
    /// request, so this is never the constructor for `api.typesafe.ai`.
    ///
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    #[must_use = "a client does nothing until it evaluates"]
    pub fn local(
        endpoint: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, TypeSafeError> {
        Ok(Self::build(None)?.with_endpoint(endpoint).with_model(model))
    }

    fn build(key: Option<String>) -> Result<Self, TypeSafeError> {
        let client = Client::builder().build()?;
        Ok(Self {
            key,
            endpoint: ENDPOINT.to_owned(),
            model: MODEL.to_owned(),
            timeout: REQUEST_TIMEOUT,
            client,
        })
    }

    /// Asks for `model` instead of the default `jev-latest`.
    ///
    /// The name travels in the request body under `model`; a backend that
    /// serves one model only ignores it.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// The model name this client asks for.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Posts to `url` instead of [`ENDPOINT`], e.g. a local fixture server in
    /// a test.
    #[must_use]
    pub fn with_endpoint(mut self, url: impl Into<String>) -> Self {
        self.endpoint = url.into();
        self
    }

    /// The URL this client posts to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Waits at most `timeout` for a whole request instead of one minute.
    ///
    /// A permission gate blocking on an answer needs to give up well before
    /// its own prompt would time out.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The timeout applied to each request.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Evaluates `questions` against `state` in a single request.
    ///
    /// # Errors
    ///
    /// When transport fails, retries are exhausted, or the API refuses.
    #[must_use = "an evaluation does nothing until its answers are read"]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "pinned public signature takes Questions by value"
    )]
    pub fn evaluate(
        &self,
        state: &State,
        questions: Questions,
    ) -> Result<Evaluation, TypeSafeError> {
        let body = request_value(state, &questions, &self.model);
        let raw = self.post(&body)?;
        parse_evaluation(&raw)
    }

    /// Asks one [`Question::Choice`] question and returns the picked option.
    ///
    /// Each criterion is an option name with its description.
    ///
    /// # Errors
    ///
    /// When the request fails or the answer is missing or misshapen.
    #[must_use = "an answer does nothing until it is read"]
    pub fn choose(
        &self,
        state: &State,
        instructions: &str,
        criteria: &[(&str, &str)],
    ) -> Result<ChoiceAnswer, TypeSafeError> {
        let evaluation =
            self.evaluate(state, choice_questions("choice", instructions, criteria))?;
        choice_answer(&evaluation)
    }

    /// Asks one [`Question::Choice`] question under a question id of your own.
    ///
    /// [`TypeSafeClient::choose`] posts under the id `choice`; this posts
    /// under `id`, for a caller whose recorded fixtures or calibration runs
    /// key on a different name.
    ///
    /// # Errors
    ///
    /// When the request fails or the answer is missing or misshapen.
    #[must_use = "an answer does nothing until it is read"]
    pub fn choose_as(
        &self,
        id: &str,
        state: &State,
        instructions: &str,
        criteria: &[(&str, &str)],
    ) -> Result<ChoiceAnswer, TypeSafeError> {
        let evaluation = self.evaluate(state, choice_questions(id, instructions, criteria))?;
        evaluation.choice(id)
    }

    /// Asks one [`Question::Noul`] question and returns the probability of yes.
    ///
    /// # Errors
    ///
    /// When the request fails or the answer is missing or misshapen.
    #[must_use = "an answer does nothing until it is read"]
    pub fn yes_no(&self, state: &State, instructions: &str) -> Result<f64, TypeSafeError> {
        let evaluation = self.evaluate(state, yes_no_questions("answer", instructions))?;
        yes_no_answer(&evaluation)
    }

    /// Asks one [`Question::Noul`] question under a question id of your own.
    ///
    /// [`TypeSafeClient::yes_no`] posts under the id `answer`; this posts
    /// under `id`.
    ///
    /// # Errors
    ///
    /// When the request fails or the answer is missing or misshapen.
    #[must_use = "an answer does nothing until it is read"]
    pub fn yes_no_as(
        &self,
        id: &str,
        state: &State,
        instructions: &str,
    ) -> Result<f64, TypeSafeError> {
        let evaluation = self.evaluate(state, yes_no_questions(id, instructions))?;
        evaluation.yes_no(id)
    }

    /// Asks one [`Question::Noul`] question and decides it against `threshold`.
    ///
    /// [`Verdict::Unsure`] comes back when the answer sits closer to the coin
    /// flip than `threshold` allows, so a caller that falls back to non-AI
    /// behavior on an uncertain answer branches on the result instead of
    /// comparing probabilities itself. See [`Verdict::from_probability`] for
    /// what the threshold measures.
    ///
    /// # Errors
    ///
    /// When the request fails or the answer is missing or misshapen.
    #[must_use = "an answer does nothing until it is read"]
    pub fn decide(
        &self,
        state: &State,
        instructions: &str,
        threshold: f64,
    ) -> Result<Verdict, TypeSafeError> {
        Ok(Verdict::from_probability(
            self.yes_no(state, instructions)?,
            threshold,
        ))
    }

    /// Asks one [`Question::Score`] question and returns the weighted position.
    ///
    /// Each level describes a concrete situation, in ascending order.
    ///
    /// # Errors
    ///
    /// When the request fails or the answer is missing or misshapen.
    #[must_use = "an answer does nothing until it is read"]
    pub fn score(
        &self,
        state: &State,
        instructions: &str,
        levels: &[&str],
    ) -> Result<ScoreAnswer, TypeSafeError> {
        let evaluation = self.evaluate(state, score_questions("score", instructions, levels))?;
        score_answer(&evaluation)
    }

    /// Asks one [`Question::Score`] question under a question id of your own.
    ///
    /// [`TypeSafeClient::score`] posts under the id `score`; this posts
    /// under `id`.
    ///
    /// # Errors
    ///
    /// When the request fails or the answer is missing or misshapen.
    #[must_use = "an answer does nothing until it is read"]
    pub fn score_as(
        &self,
        id: &str,
        state: &State,
        instructions: &str,
        levels: &[&str],
    ) -> Result<ScoreAnswer, TypeSafeError> {
        let evaluation = self.evaluate(state, score_questions(id, instructions, levels))?;
        evaluation.score(id)
    }

    fn post(&self, body: &Value) -> Result<String, TypeSafeError> {
        let mut attempt: u32 = 0;
        loop {
            let mut request = self
                .client
                .post(&self.endpoint)
                .timeout(self.timeout)
                .json(body);
            if let Some(key) = &self.key {
                request = request.bearer_auth(key);
            }
            let response = request.send()?;
            let status = response.status();
            if is_retryable(status) {
                if attempt + 1 >= MAX_ATTEMPTS {
                    return Err(error_from_text(status.as_u16(), &response_text(response)));
                }
                thread::sleep(retry_delay(attempt));
                attempt += 1;
            } else if status.is_success() {
                return response.text().map_err(TypeSafeError::from);
            } else {
                return Err(error_from_text(status.as_u16(), &response_text(response)));
            }
        }
    }
}

fn key_from_env() -> Result<String, TypeSafeError> {
    env::var("TYPESAFE_API_KEY").map_err(|_| {
        TypeSafeError::MissingKey("environment variable TYPESAFE_API_KEY is missing".to_owned())
    })
}

fn key_from_path(path: impl AsRef<Path>) -> Result<String, TypeSafeError> {
    let content = fs::read_to_string(path)?;
    Ok(content.lines().next().unwrap_or("").to_owned())
}

fn checked_key(key: &str) -> Result<String, TypeSafeError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(TypeSafeError::MissingKey("API key is blank".to_owned()));
    }
    Ok(trimmed.to_owned())
}

fn choice_questions(id: &str, instructions: &str, criteria: &[(&str, &str)]) -> Questions {
    let mut questions = Questions::new();
    questions.insert(id.to_owned(), Question::choice(instructions, criteria));
    questions
}

fn choice_answer(evaluation: &Evaluation) -> Result<ChoiceAnswer, TypeSafeError> {
    evaluation.choice("choice")
}

fn yes_no_questions(id: &str, instructions: &str) -> Questions {
    let mut questions = Questions::new();
    questions.insert(id.to_owned(), Question::yes_no(instructions));
    questions
}

fn yes_no_answer(evaluation: &Evaluation) -> Result<f64, TypeSafeError> {
    evaluation.yes_no("answer")
}

fn score_questions(id: &str, instructions: &str, levels: &[&str]) -> Questions {
    let mut questions = Questions::new();
    questions.insert(id.to_owned(), Question::score(instructions, levels));
    questions
}

fn score_answer(evaluation: &Evaluation) -> Result<ScoreAnswer, TypeSafeError> {
    evaluation.score("score")
}

fn request_value(state: &State, questions: &Questions, model: &str) -> Value {
    serde_json::json!({
        "state": state.to_value(),
        "model": model,
        "questions": questions,
    })
}

/// Builds the JSON body of a System One request, as compact text.
///
/// Exposed for tests of the wire format; not part of the supported API.
#[doc(hidden)]
#[must_use]
pub fn request_body(state: &State, questions: &Questions) -> String {
    request_value(state, questions, MODEL).to_string()
}

/// Builds the JSON body of a request naming `model`, as compact text.
///
/// Exposed for tests of the wire format; not part of the supported API.
#[doc(hidden)]
#[must_use]
pub fn request_body_for(state: &State, questions: &Questions, model: &str) -> String {
    request_value(state, questions, model).to_string()
}

/// Parses a System One success body given as text into an [`Evaluation`].
///
/// Exposed for tests of the wire format; not part of the supported API.
///
/// # Errors
///
/// When the body does not match the documented response shape.
#[doc(hidden)]
#[must_use = "a parsed evaluation does nothing until it is read"]
pub fn parse_evaluation(body: &str) -> Result<Evaluation, TypeSafeError> {
    serde_json::from_str(body).map_err(|error| TypeSafeError::InvalidResponse(error.to_string()))
}

/// Maps an error status plus raw body text to the typed [`TypeSafeError::Api`].
///
/// Exposed for tests of the wire format; not part of the supported API.
#[doc(hidden)]
#[must_use]
pub fn error_from_text(status: u16, text: &str) -> TypeSafeError {
    let body: Value = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()));
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .map_or_else(|| body.to_string(), str::to_owned);
    TypeSafeError::Api { status, message }
}

fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 529
}

const fn retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(BACKOFF_BASE_MS << attempt)
}

fn response_text(response: Response) -> String {
    response.text().unwrap_or_else(|_| String::new())
}

mod test {
    use super::{State, TypeSafeClient};
    #[test]
    fn test_filter() {
        dotenv::dotenv().ok();
        let client = TypeSafeClient::from_env().unwrap();
        let state = State::text("The door creaks open.");
        let chance: f64 = client.yes_no(&state, "Is the door open?").unwrap();
        println!("Chance: {}", chance)
    }
}
