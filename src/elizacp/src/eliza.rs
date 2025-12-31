//! ELIZA chatbot implementation based on Weizenbaum's 1966 algorithm.
//!
//! This module implements the classic ELIZA pattern-matching algorithm
//! as described in the January 1966 CACM paper, with a modernized script
//! format (TOML instead of S-expressions) and normal case text.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::Deserialize;
use std::collections::HashMap;

// =============================================================================
// Data Structures
// =============================================================================

/// A decomposition pattern element
#[derive(Debug, Clone)]
enum PatternElement {
    /// Match this exact word (case-insensitive)
    Exact(String),
    /// Match exactly N words
    Count(usize),
    /// Match N to M words (inclusive)
    Range(usize, usize),
    /// Match 0 or more words (wildcard)
    Wildcard,
    /// Match any of these words
    AnyOf(Vec<String>),
    /// Match any word with this tag
    Tag(String),
}

/// A reassembly rule element
#[derive(Debug, Clone)]
enum ReassemblyElement {
    /// Literal text
    Text(String),
    /// Reference to matched component (1-indexed), no reflection
    ComponentRef(usize),
    /// Reference to matched component (1-indexed), with pronoun reflection
    ComponentRefReflect(usize),
}

/// Special reassembly actions
#[derive(Debug, Clone)]
enum ReassemblyAction {
    /// Normal reassembly with elements
    Reassemble(Vec<ReassemblyElement>),
    /// Jump to another keyword
    Goto(String),
    /// Try next keyword in stack
    Newkey,
}

/// A decomposition/reassembly pair
#[derive(Debug, Clone)]
struct Transform {
    decomposition: Vec<PatternElement>,
    reassembly_rules: Vec<ReassemblyAction>,
}

/// A keyword rule
#[derive(Debug, Clone)]
struct Keyword {
    /// Fix typos/contractions - applied during tokenization (e.g., "dont" -> "don't")
    repair: Option<String>,
    /// Priority for keyword selection (higher = more important)
    precedence: u32,
    /// Direct link to another keyword (equivalence class)
    link: Option<String>,
    /// Transformation rules
    transforms: Vec<Transform>,
}

/// Memory rule for storing and recalling statements
#[derive(Debug, Clone)]
struct MemoryRule {
    /// The keyword that triggers memory creation (usually "my")
    keyword: String,
    /// Exactly 4 transforms for memory creation
    transforms: Vec<Transform>,
}

/// Pronoun reflections (I -> you, my -> your, etc.)
#[derive(Debug, Clone)]
struct Reflections {
    map: HashMap<String, String>,
}

/// The complete ELIZA script (immutable after loading)
#[derive(Debug, Clone)]
struct Script {
    hello_message: String,
    keywords: HashMap<String, Keyword>,
    memory_rule: Option<MemoryRule>,
    /// Maps tag names to words that have that tag
    tags: HashMap<String, Vec<String>>,
    reflections: Reflections,
}

/// Mutable runtime state for ELIZA
#[derive(Debug, Clone)]
struct State {
    /// LIMIT counter (1-4, affects memory recall)
    limit: u8,
    /// Cycling state for reassembly rules: (keyword, transform_index) -> next_rule
    rule_indices: HashMap<(String, usize), usize>,
    /// Queue of stored memories for later recall
    memories: Vec<String>,
}

// =============================================================================
// TOML Schema
// =============================================================================

#[derive(Debug, Deserialize)]
struct TomlScript {
    hello: String,
    #[serde(default)]
    keywords: Vec<TomlKeyword>,
    memory: Option<TomlMemory>,
}

#[derive(Debug, Deserialize)]
struct TomlKeyword {
    word: String,
    /// Fix typos/contractions - applied during tokenization (e.g., "dont" -> "don't")
    #[serde(default)]
    repair: Option<String>,
    /// Pronoun reflection - only used during reassembly with {N:r} (e.g., "you" -> "i")
    #[serde(default)]
    reflection: Option<String>,
    #[serde(default)]
    precedence: u32,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    transforms: Vec<TomlTransform>,
}

#[derive(Debug, Deserialize)]
struct TomlTransform {
    decomposition: Vec<String>,
    #[serde(default)]
    reassembly: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TomlMemory {
    keyword: String,
    transforms: Vec<TomlMemoryTransform>,
}

#[derive(Debug, Deserialize)]
struct TomlMemoryTransform {
    decomposition: Vec<String>,
    reassembly: String,
}

// =============================================================================
// Parsing
// =============================================================================

impl PatternElement {
    fn parse(s: &str) -> Self {
        let s = s.trim();

        // Wildcard: ".."
        if s == ".." {
            return PatternElement::Wildcard;
        }

        // Range: "1..3"
        if let Some((left, right)) = s.split_once("..") {
            if let (Ok(n), Ok(m)) = (left.parse::<usize>(), right.parse::<usize>()) {
                return PatternElement::Range(n, m);
            }
        }

        // Exact count: "2"
        if let Ok(n) = s.parse::<usize>() {
            return PatternElement::Count(n);
        }

        // Tag: "#FAMILY"
        if let Some(tag) = s.strip_prefix('#') {
            return PatternElement::Tag(tag.to_lowercase());
        }

        // AnyOf: "{WANT, NEED}"
        if s.starts_with('{') && s.ends_with('}') {
            let inner = &s[1..s.len() - 1];
            let words: Vec<String> = inner
                .split(',')
                .map(|w| w.trim().to_lowercase())
                .filter(|w| !w.is_empty())
                .collect();
            return PatternElement::AnyOf(words);
        }

        // Exact word match
        PatternElement::Exact(s.to_lowercase())
    }
}

impl ReassemblyAction {
    fn parse(s: &str) -> Self {
        let s = s.trim();

        // Goto: "=WHAT"
        if let Some(target) = s.strip_prefix('=') {
            return ReassemblyAction::Goto(target.to_lowercase());
        }

        // Newkey
        if s.eq_ignore_ascii_case("NEWKEY") {
            return ReassemblyAction::Newkey;
        }

        // Normal reassembly - parse for {N} references
        let mut elements = Vec::new();
        let mut current_text = String::new();
        let mut chars = s.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '{' {
                // Might be a component reference: {N} or {N:r}
                let mut num_str = String::new();
                let mut reflect = false;

                while let Some(&next) = chars.peek() {
                    if next == '}' {
                        chars.next();
                        break;
                    }
                    if next.is_ascii_digit() {
                        num_str.push(chars.next().unwrap());
                    } else if next == ':' && !num_str.is_empty() {
                        // Check for :r suffix
                        chars.next(); // consume ':'
                        if chars.peek() == Some(&'r') {
                            chars.next(); // consume 'r'
                            reflect = true;
                        }
                    } else {
                        // Not a valid reference, treat as literal
                        current_text.push('{');
                        current_text.push_str(&num_str);
                        num_str.clear();
                        break;
                    }
                }
                if !num_str.is_empty() {
                    if let Ok(n) = num_str.parse::<usize>() {
                        if !current_text.is_empty() {
                            elements.push(ReassemblyElement::Text(current_text.clone()));
                            current_text.clear();
                        }
                        if reflect {
                            elements.push(ReassemblyElement::ComponentRefReflect(n));
                        } else {
                            elements.push(ReassemblyElement::ComponentRef(n));
                        }
                        continue;
                    }
                }
            } else {
                current_text.push(c);
            }
        }
        if !current_text.is_empty() {
            elements.push(ReassemblyElement::Text(current_text));
        }

        ReassemblyAction::Reassemble(elements)
    }
}

impl Script {
    fn from_toml(toml_str: &str) -> Result<Self, toml::de::Error> {
        let toml_script: TomlScript = toml::from_str(toml_str)?;

        let mut keywords = HashMap::new();
        let mut tags: HashMap<String, Vec<String>> = HashMap::new();
        let mut reflections_from_script: HashMap<String, String> = HashMap::new();

        for tk in toml_script.keywords {
            let word = tk.word.to_lowercase();

            // Collect tags
            for tag in &tk.tags {
                tags.entry(tag.to_lowercase())
                    .or_default()
                    .push(word.clone());
            }

            let transforms: Vec<Transform> = tk
                .transforms
                .into_iter()
                .map(|t| Transform {
                    decomposition: t
                        .decomposition
                        .iter()
                        .map(|s| PatternElement::parse(s))
                        .collect(),
                    reassembly_rules: t
                        .reassembly
                        .iter()
                        .map(|s| ReassemblyAction::parse(s))
                        .collect(),
                })
                .collect();

            let keyword = Keyword {
                repair: tk.repair.map(|s| s.to_lowercase()),
                precedence: tk.precedence,
                link: tk.link.map(|s| s.to_lowercase()),
                transforms,
            };

            // Build reflection map from keywords that have reflection defined
            if let Some(ref reflection) = tk.reflection {
                reflections_from_script.insert(word.clone(), reflection.to_lowercase());
            }

            keywords.insert(word, keyword);
        }

        let memory_rule = toml_script.memory.map(|m| {
            let transforms = m
                .transforms
                .into_iter()
                .map(|t| Transform {
                    decomposition: t
                        .decomposition
                        .iter()
                        .map(|s| PatternElement::parse(s))
                        .collect(),
                    reassembly_rules: vec![ReassemblyAction::parse(&t.reassembly)],
                })
                .collect();
            MemoryRule {
                keyword: m.keyword.to_lowercase(),
                transforms,
            }
        });

        // Merge script reflections with defaults (script takes precedence)
        let mut reflections = Reflections::default();
        for (word, reflection) in reflections_from_script {
            reflections.map.insert(word, reflection);
        }

        Ok(Script {
            hello_message: toml_script.hello,
            keywords,
            memory_rule,
            tags,
            reflections,
        })
    }
}

impl Default for Reflections {
    fn default() -> Self {
        let mut map = HashMap::new();

        // First person -> second person
        map.insert("i".to_string(), "you".to_string());
        map.insert("i'm".to_string(), "you're".to_string());
        map.insert("i've".to_string(), "you've".to_string());
        map.insert("i'll".to_string(), "you'll".to_string());
        map.insert("i'd".to_string(), "you'd".to_string());
        map.insert("me".to_string(), "you".to_string());
        map.insert("my".to_string(), "your".to_string());
        map.insert("mine".to_string(), "yours".to_string());
        map.insert("myself".to_string(), "yourself".to_string());
        map.insert("am".to_string(), "are".to_string());
        map.insert("we".to_string(), "you".to_string());
        map.insert("our".to_string(), "your".to_string());
        map.insert("ours".to_string(), "yours".to_string());
        map.insert("ourselves".to_string(), "yourselves".to_string());
        map.insert("was".to_string(), "were".to_string());

        // Second person -> first person
        map.insert("you".to_string(), "I".to_string());
        map.insert("you're".to_string(), "I'm".to_string());
        map.insert("you've".to_string(), "I've".to_string());
        map.insert("you'll".to_string(), "I'll".to_string());
        map.insert("you'd".to_string(), "I'd".to_string());
        map.insert("your".to_string(), "my".to_string());
        map.insert("yours".to_string(), "mine".to_string());
        map.insert("yourself".to_string(), "myself".to_string());

        Self { map }
    }
}

impl Reflections {
    fn reflect(&self, text: &str) -> String {
        text.split_whitespace()
            .map(|word| {
                let lower = word.to_lowercase();
                // Strip punctuation for lookup
                let (base, punct) = if lower.ends_with(|c: char| c.is_ascii_punctuation()) {
                    let punct = &lower[lower.len() - 1..];
                    (&lower[..lower.len() - 1], punct)
                } else {
                    (lower.as_str(), "")
                };

                if let Some(reflected) = self.map.get(base) {
                    format!("{}{}", reflected, punct)
                } else {
                    word.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// =============================================================================
// Core Algorithm
// =============================================================================

/// Result of attempting a transformation
enum TransformResult {
    /// Successfully generated a response
    Complete(String),
    /// Follow a link to another keyword
    Goto(String, Vec<String>),
    /// Try the next keyword
    Newkey,
    /// No decomposition matched
    NoMatch,
}

impl Script {
    /// Try to match a decomposition pattern against words
    fn match_pattern(&self, pattern: &[PatternElement], words: &[String]) -> Option<Vec<String>> {
        self.match_pattern_recursive(pattern, words, Vec::new())
    }

    fn match_pattern_recursive(
        &self,
        pattern: &[PatternElement],
        words: &[String],
        mut components: Vec<String>,
    ) -> Option<Vec<String>> {
        if pattern.is_empty() {
            return if words.is_empty() {
                Some(components)
            } else {
                None
            };
        }

        let elem = &pattern[0];
        let rest_pattern = &pattern[1..];

        match elem {
            PatternElement::Exact(expected) => {
                if words.is_empty() || words[0].to_lowercase() != *expected {
                    return None;
                }
                components.push(words[0].clone());
                self.match_pattern_recursive(rest_pattern, &words[1..], components)
            }

            PatternElement::Count(n) => {
                if words.len() < *n {
                    return None;
                }
                components.push(words[..*n].join(" "));
                self.match_pattern_recursive(rest_pattern, &words[*n..], components)
            }

            PatternElement::Range(min, max) => {
                for n in (*min..=*max).rev() {
                    if words.len() >= n {
                        let mut new_components = components.clone();
                        new_components.push(words[..n].join(" "));
                        if let Some(result) =
                            self.match_pattern_recursive(rest_pattern, &words[n..], new_components)
                        {
                            return Some(result);
                        }
                    }
                }
                None
            }

            PatternElement::Wildcard => {
                for n in 0..=words.len() {
                    let mut new_components = components.clone();
                    new_components.push(words[..n].join(" "));
                    if let Some(result) =
                        self.match_pattern_recursive(rest_pattern, &words[n..], new_components)
                    {
                        return Some(result);
                    }
                }
                None
            }

            PatternElement::AnyOf(options) => {
                if words.is_empty() {
                    return None;
                }
                let word_lower = words[0].to_lowercase();
                if !options.contains(&word_lower) {
                    return None;
                }
                components.push(words[0].clone());
                self.match_pattern_recursive(rest_pattern, &words[1..], components)
            }

            PatternElement::Tag(tag) => {
                if words.is_empty() {
                    return None;
                }
                let word_lower = words[0].to_lowercase();
                if !self
                    .tags
                    .get(tag)
                    .map_or(false, |tw| tw.contains(&word_lower))
                {
                    return None;
                }
                components.push(words[0].clone());
                self.match_pattern_recursive(rest_pattern, &words[1..], components)
            }
        }
    }

    /// Reassemble a response from a rule and matched components
    fn reassemble(&self, elements: &[ReassemblyElement], components: &[String]) -> String {
        let mut result = String::new();
        for elem in elements {
            match elem {
                ReassemblyElement::Text(text) => {
                    result.push_str(text);
                }
                ReassemblyElement::ComponentRef(n) => {
                    if *n > 0 && *n <= components.len() {
                        result.push_str(&components[*n - 1]);
                    }
                }
                ReassemblyElement::ComponentRefReflect(n) => {
                    if *n > 0 && *n <= components.len() {
                        let reflected = self.reflections.reflect(&components[*n - 1]);
                        result.push_str(&reflected);
                    }
                }
            }
        }
        result
    }

    /// Apply transformation for a keyword
    fn apply_transform(
        &self,
        state: &mut State,
        keyword_name: &str,
        words: &[String],
    ) -> TransformResult {
        let keyword = match self.keywords.get(keyword_name) {
            Some(k) => k,
            None => return TransformResult::NoMatch,
        };

        // If keyword has a direct link and no transforms, follow it
        if keyword.transforms.is_empty() {
            if let Some(ref link) = keyword.link {
                return TransformResult::Goto(link.clone(), words.to_vec());
            }
            return TransformResult::NoMatch;
        }

        // Try each transform's decomposition
        for (transform_idx, transform) in keyword.transforms.iter().enumerate() {
            if let Some(components) = self.match_pattern(&transform.decomposition, words) {
                if transform.reassembly_rules.is_empty() {
                    // No reassembly rules - check for link
                    if let Some(ref link) = keyword.link {
                        return TransformResult::Goto(link.clone(), words.to_vec());
                    }
                    continue;
                }

                // Get next reassembly rule (cycling)
                let state_key = (keyword_name.to_string(), transform_idx);
                let rule_idx = *state.rule_indices.get(&state_key).unwrap_or(&0);
                let next_idx = (rule_idx + 1) % transform.reassembly_rules.len();
                state.rule_indices.insert(state_key, next_idx);

                match &transform.reassembly_rules[rule_idx] {
                    ReassemblyAction::Reassemble(elements) => {
                        let response = self.reassemble(elements, &components);
                        return TransformResult::Complete(response);
                    }
                    ReassemblyAction::Goto(target) => {
                        return TransformResult::Goto(target.clone(), words.to_vec());
                    }
                    ReassemblyAction::Newkey => {
                        return TransformResult::Newkey;
                    }
                }
            }
        }

        // No decomposition matched - check for link
        if let Some(ref link) = keyword.link {
            return TransformResult::Goto(link.clone(), words.to_vec());
        }

        TransformResult::NoMatch
    }

    /// Try to create a memory from input
    fn try_create_memory(&self, state: &mut State, words: &[String]) {
        let memory_rule = match &self.memory_rule {
            Some(m) => m,
            None => return,
        };

        // Check if memory keyword is in the words
        if !words
            .iter()
            .any(|w| w.to_lowercase() == memory_rule.keyword)
        {
            return;
        }

        // Use a hash-like selection for which transform to use
        let transform_idx = if !words.is_empty() {
            let last_word = words.last().unwrap();
            let hash: usize = last_word.bytes().map(|b| b as usize).sum();
            hash % memory_rule.transforms.len().max(1)
        } else {
            0
        };

        if transform_idx >= memory_rule.transforms.len() {
            return;
        }

        let transform = &memory_rule.transforms[transform_idx];
        if let Some(components) = self.match_pattern(&transform.decomposition, words) {
            if let Some(ReassemblyAction::Reassemble(elements)) = transform.reassembly_rules.first()
            {
                let memory = self.reassemble(elements, &components);
                state.memories.push(memory);
            }
        }
    }
}

// =============================================================================
// Public API
// =============================================================================

/// The Eliza chatbot engine.
#[derive(Clone)]
pub struct Eliza {
    script: Script,
    state: State,
}

impl Eliza {
    /// Create a new Eliza instance with a random seed.
    pub fn new() -> Self {
        Self::with_seed(rand::random())
    }

    /// Create a new Eliza instance with a fixed seed for deterministic behavior.
    pub fn new_deterministic() -> Self {
        Self::with_seed(22)
    }

    /// Create a new Eliza instance with a specific seed.
    pub fn with_seed(seed: u64) -> Self {
        let script = Script::from_toml(DOCTOR_SCRIPT).expect("Failed to parse built-in script");

        // Initialize state with randomized starting positions for reassembly rule cycling
        // Sort keywords by name for deterministic iteration order
        let mut rng = StdRng::seed_from_u64(seed);
        let mut rule_indices = HashMap::new();
        let mut keyword_names: Vec<_> = script.keywords.keys().collect();
        keyword_names.sort();
        for keyword_name in keyword_names {
            let keyword = &script.keywords[keyword_name];
            for (transform_idx, transform) in keyword.transforms.iter().enumerate() {
                if !transform.reassembly_rules.is_empty() {
                    let start = rng.random_range(0..transform.reassembly_rules.len());
                    rule_indices.insert((keyword_name.clone(), transform_idx), start);
                }
            }
        }

        Self {
            script,
            state: State {
                limit: 0,
                rule_indices,
                memories: Vec::new(),
            },
        }
    }

    /// Get the hello message
    pub fn hello(&self) -> &str {
        &self.script.hello_message
    }

    /// Generate a response to user input.
    pub fn respond(&mut self, input: &str) -> String {
        let input = input.trim();
        if input.is_empty() {
            return "Please tell me more.".to_string();
        }

        // Update LIMIT counter (cycles 1-4)
        self.state.limit = (self.state.limit % 4) + 1;

        // Tokenize and normalize
        let words = self.tokenize(input);
        if words.is_empty() {
            return "Please tell me more.".to_string();
        }

        // Process the input
        self.process_input(&words)
    }

    /// Tokenize input into words
    fn tokenize(&self, input: &str) -> Vec<String> {
        input
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | '?' | '!' | ';' | ':'))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    /// Process input and generate response
    fn process_input(&mut self, original_words: &[String]) -> String {
        // Apply repairs and build keystack (but NOT substitutions - those happen during reassembly)
        let mut words = Vec::new();
        let mut keystack: Vec<(String, u32)> = Vec::new();

        for word in original_words {
            let word_lower = word.to_lowercase();

            if let Some(keyword) = self.script.keywords.get(&word_lower) {
                // Apply repair if any (e.g., "dont" -> "don't")
                let repaired = keyword.repair.clone().unwrap_or_else(|| word_lower.clone());
                words.push(repaired);

                // Add to keystack if it has transforms or a link
                if !keyword.transforms.is_empty() || keyword.link.is_some() {
                    keystack.push((word_lower.clone(), keyword.precedence));
                }
            } else {
                words.push(word_lower.clone());
            }
        }

        // Sort keystack by precedence (highest first)
        keystack.sort_by(|a, b| b.1.cmp(&a.1));

        // If no keywords found, check memory or use NONE
        if keystack.is_empty() {
            // Check for memory recall (LIMIT=4 and memories exist)
            if self.state.limit == 4 && !self.state.memories.is_empty() {
                return self.state.memories.remove(0); // FIFO queue
            }

            // Use NONE keyword
            return self.apply_none(&words);
        }

        // Try to create a memory if the memory keyword is present
        self.script.try_create_memory(&mut self.state, &words);

        // Process keywords in order
        let mut visited_keywords = Vec::new();
        for (keyword_name, _) in &keystack {
            if let Some(response) = self.apply_keyword(keyword_name, &words, &mut visited_keywords)
            {
                return response;
            }
        }

        // Fallback to NONE
        self.apply_none(&words)
    }

    /// Apply a keyword transformation, following links as needed
    fn apply_keyword(
        &mut self,
        keyword_name: &str,
        words: &[String],
        visited: &mut Vec<String>,
    ) -> Option<String> {
        // Prevent infinite loops
        if visited.contains(&keyword_name.to_string()) {
            return None;
        }
        visited.push(keyword_name.to_string());

        // Limit link following depth
        if visited.len() > 10 {
            return None;
        }

        match self
            .script
            .apply_transform(&mut self.state, keyword_name, words)
        {
            TransformResult::Complete(response) => Some(response),
            TransformResult::Goto(target, new_words) => {
                self.apply_keyword(&target, &new_words, visited)
            }
            TransformResult::Newkey => None,
            TransformResult::NoMatch => None,
        }
    }

    /// Apply the NONE keyword (fallback)
    fn apply_none(&mut self, words: &[String]) -> String {
        if let Some(result) = self.apply_keyword("none", words, &mut Vec::new()) {
            return result;
        }
        "I'm not sure I understand you fully.".to_string()
    }
}

impl Default for Eliza {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Eliza {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Eliza")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

// =============================================================================
// Built-in DOCTOR Script
// =============================================================================

const DOCTOR_SCRIPT: &str = r##"
hello = "How do you do. Please tell me your problem."

# Word substitutions (applied during input processing)
[[keywords]]
word = "dont"
repair = "don't"

[[keywords]]
word = "cant"
repair = "can't"

[[keywords]]
word = "wont"
repair = "won't"

[[keywords]]
word = "dreamed"
repair = "dreamt"
link = "dreamt"

# SORRY
[[keywords]]
word = "sorry"
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Please don't apologize.",
    "Apologies are not necessary.",
    "What feelings do you have when you apologize?",
    "I've told you that apologies are not required.",
]

# REMEMBER
[[keywords]]
word = "remember"
precedence = 5
[[keywords.transforms]]
decomposition = ["..", "i", "remember", ".."]
reassembly = [
    "Do you often think of {4}?",
    "Does thinking of {4} bring anything else to mind?",
    "What else do you remember?",
    "Why do you remember {4} just now?",
    "What in the present situation reminds you of {4}?",
    "What is the connection between me and {4}?",
]
[[keywords.transforms]]
decomposition = ["..", "do", "i", "remember", ".."]
reassembly = [
    "Did you think I would forget {5}?",
    "Why do you think I should recall {5} now?",
    "What about {5}?",
    "=what",
    "You mentioned {5}.",
]
[[keywords.transforms]]
decomposition = [".."]
reassembly = ["NEWKEY"]

# IF
[[keywords]]
word = "if"
precedence = 3
[[keywords.transforms]]
decomposition = ["..", "if", ".."]
reassembly = [
    "Do you think it's likely that {3}?",
    "Do you wish that {3}?",
    "What do you think about {3}?",
    "Really, {2} {3}?",
]

# DREAMT
[[keywords]]
word = "dreamt"
precedence = 4
[[keywords.transforms]]
decomposition = ["..", "i", "dreamt", ".."]
reassembly = [
    "Really, {4}?",
    "Have you ever fantasized {4} while you were awake?",
    "Have you dreamt {4} before?",
    "=dream",
    "NEWKEY",
]

# DREAM
[[keywords]]
word = "dream"
precedence = 3
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "What does that dream suggest to you?",
    "Do you dream often?",
    "What persons appear in your dreams?",
    "Don't you believe that dream has something to do with your problem?",
    "NEWKEY",
]

[[keywords]]
word = "dreams"
link = "dream"

# ALIKE / SAME
[[keywords]]
word = "alike"
precedence = 10
link = "dit"

[[keywords]]
word = "same"
precedence = 10
link = "dit"

# CERTAINLY
[[keywords]]
word = "certainly"
link = "yes"

# PERHAPS
[[keywords]]
word = "perhaps"
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "You don't seem quite certain.",
    "Why the uncertain tone?",
    "Can't you be more positive?",
    "You aren't sure.",
    "Don't you know?",
]

[[keywords]]
word = "maybe"
link = "perhaps"

# NAME
[[keywords]]
word = "name"
precedence = 15
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "I am not interested in names.",
    "I've told you before, I don't care about names. Please continue.",
]

# HELLO
[[keywords]]
word = "hello"
[[keywords.transforms]]
decomposition = [".."]
reassembly = ["How do you do. Please state your problem."]

[[keywords]]
word = "hi"
link = "hello"

# COMPUTER
[[keywords]]
word = "computer"
precedence = 50
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Do computers worry you?",
    "Why do you mention computers?",
    "What do you think machines have to do with your problem?",
    "Don't you think computers can help people?",
    "What about machines worries you?",
    "What do you think about machines?",
]

[[keywords]]
word = "machine"
precedence = 50
link = "computer"

[[keywords]]
word = "machines"
precedence = 50
link = "computer"

[[keywords]]
word = "computers"
precedence = 50
link = "computer"

# AM (user says "am I...")
[[keywords]]
word = "am"
reflection = "are"
[[keywords.transforms]]
decomposition = ["..", "am", "i", ".."]
reassembly = [
    "Do you believe you are {4}?",
    "Would you want to be {4}?",
    "You wish I would tell you you are {4}.",
    "What would it mean if you were {4}?",
    "=what",
]
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Why do you say 'am'?",
    "I don't understand that.",
]

# ARE
[[keywords]]
word = "are"
[[keywords.transforms]]
decomposition = ["..", "are", "i", ".."]
reassembly = [
    "Why are you interested in whether I am {4} or not?",
    "Would you prefer if I weren't {4}?",
    "Perhaps I am {4} in your fantasies.",
    "Do you sometimes think I am {4}?",
    "=what",
]
[[keywords.transforms]]
decomposition = ["..", "are", ".."]
reassembly = [
    "Did you think they might not be {3}?",
    "Would you like it if they were not {3}?",
    "What if they were not {3}?",
    "Possibly they are {3}.",
]

# YOUR (user says "your" referring to Eliza)
[[keywords]]
word = "your"
reflection = "my"
[[keywords.transforms]]
decomposition = ["..", "your", ".."]
reassembly = [
    "Why are you concerned over my {3}?",
    "What about your own {3}?",
    "Are you worried about someone else's {3}?",
    "Really, my {3}?",
]

# WAS
[[keywords]]
word = "was"
precedence = 2
[[keywords.transforms]]
decomposition = ["..", "was", "i", ".."]
reassembly = [
    "What if you were {4}?",
    "Do you think you were {4}?",
    "Were you {4}?",
    "What would it mean if you were {4}?",
    "What does '{4}' suggest to you?",
    "=what",
]
[[keywords.transforms]]
decomposition = ["..", "i", "was", ".."]
reassembly = [
    "Were you really?",
    "Why do you tell me you were {4} now?",
    "Perhaps I already knew you were {4}.",
]
[[keywords.transforms]]
decomposition = ["..", "was", "you", ".."]
reassembly = [
    "Would you like to believe I was {4}?",
    "What suggests that I was {4}?",
    "What do you think?",
    "Perhaps I was {4}.",
    "What if I had been {4}?",
]
[[keywords.transforms]]
decomposition = [".."]
reassembly = ["NEWKEY"]

[[keywords]]
word = "were"
link = "was"

# I (user saying I)
[[keywords]]
word = "i"
reflection = "you"
[[keywords.transforms]]
decomposition = ["..", "i", "{want, need}", ".."]
reassembly = [
    "What would it mean to you if you got {4}?",
    "Why do you want {4}?",
    "Suppose you got {4} soon.",
    "What if you never got {4}?",
    "What would getting {4} mean to you?",
    "What does wanting {4} have to do with this discussion?",
]
[[keywords.transforms]]
decomposition = ["..", "i", "am", "..", "{sad, unhappy, depressed, sick}", ".."]
reassembly = [
    "I am sorry to hear you are {5}.",
    "Do you think coming here will help you not to be {5}?",
    "I'm sure it's not pleasant to be {5}.",
    "Can you explain what made you {5}?",
]
[[keywords.transforms]]
decomposition = ["..", "i", "am", "..", "{happy, elated, glad, better}", ".."]
reassembly = [
    "How have I helped you to be {5}?",
    "Has your treatment made you {5}?",
    "What makes you {5} just now?",
    "Can you explain why you are suddenly {5}?",
]
[[keywords.transforms]]
decomposition = ["..", "i", "was", ".."]
reassembly = ["=was"]
[[keywords.transforms]]
decomposition = ["..", "i", "#belief", "i", ".."]
reassembly = [
    "Do you really think so?",
    "But you are not sure you {5}.",
    "Do you really doubt you {5}?",
]
[[keywords.transforms]]
decomposition = ["..", "i", "..", "#belief", "..", "you", ".."]
reassembly = ["=you"]
[[keywords.transforms]]
decomposition = ["..", "i", "am", ".."]
reassembly = [
    "Is it because you are {4} that you came to me?",
    "How long have you been {4}?",
    "Do you believe it is normal to be {4}?",
    "Do you enjoy being {4}?",
]
[[keywords.transforms]]
decomposition = ["..", "i", "{can't, cannot}", ".."]
reassembly = [
    "How do you know you can't {4}?",
    "Have you tried?",
    "Perhaps you could {4} now.",
    "Do you really want to be able to {4}?",
]
[[keywords.transforms]]
decomposition = ["..", "i", "don't", ".."]
reassembly = [
    "Don't you really {4}?",
    "Why don't you {4}?",
    "Do you wish to be able to {4}?",
    "Does that trouble you?",
]
[[keywords.transforms]]
decomposition = ["..", "i", "feel", ".."]
reassembly = [
    "Tell me more about such feelings.",
    "Do you often feel {4}?",
    "Do you enjoy feeling {4}?",
    "Of what does feeling {4} remind you?",
]
[[keywords.transforms]]
decomposition = ["..", "i", "..", "you", ".."]
reassembly = [
    "Perhaps in your fantasy we {3} each other.",
    "Do you wish to {3} me?",
    "You seem to need to {3} me.",
    "Do you {3} anyone else?",
]
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "You say {1}.",
    "Can you elaborate on that?",
    "Do you say {1} for some special reason?",
    "That's quite interesting.",
]

# YOU (user said "you" referring to Eliza)
[[keywords]]
word = "you"
reflection = "i"
[[keywords.transforms]]
decomposition = ["..", "you", "remind", "me", "of", ".."]
reassembly = ["=dit"]
[[keywords.transforms]]
decomposition = ["..", "you", "are", ".."]
reassembly = [
    "What makes you think I am {4}?",
    "Does it please you to believe I am {4}?",
    "Do you sometimes wish you were {4}?",
    "Perhaps you would like to be {4}.",
]
[[keywords.transforms]]
decomposition = ["..", "you", "..", "me"]
reassembly = [
    "Why do you think I {3} you?",
    "You like to think I {3} you, don't you?",
    "What makes you think I {3} you?",
    "Really, I {3} you?",
    "Do you wish to believe I {3} you?",
    "Suppose I did {3} you. What would that mean?",
    "Does someone else believe I {3} you?",
]
[[keywords.transforms]]
decomposition = ["..", "you", ".."]
reassembly = [
    "We were discussing you, not me.",
    "Oh, I {3}?",
    "You're not really talking about me, are you?",
    "What are your feelings now?",
]

# YES
[[keywords]]
word = "yes"
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "You seem quite positive.",
    "You are sure.",
    "I see.",
    "I understand.",
]

# NO
[[keywords]]
word = "no"
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Are you saying 'no' just to be negative?",
    "You are being a bit negative.",
    "Why not?",
    "Why 'no'?",
]

# MY (user says "my")
[[keywords]]
word = "my"
reflection = "your"
precedence = 2
[[keywords.transforms]]
decomposition = ["..", "my", "..", "#family", ".."]
reassembly = [
    "Tell me more about your family.",
    "Who else in your family {5}?",
    "Your {4} {5}?",
    "What else comes to mind when you think of your {4}?",
]
[[keywords.transforms]]
decomposition = ["..", "my", ".."]
reassembly = [
    "Your {3}, you say?",
    "Why do you say your {3}?",
    "Does that suggest anything else which belongs to you?",
    "Is it important to you that {2} {3}?",
]

# CAN
[[keywords]]
word = "can"
[[keywords.transforms]]
decomposition = ["..", "can", "i", ".."]
reassembly = [
    "You believe I can {4}, don't you?",
    "=what",
    "You want me to be able to {4}.",
    "Perhaps you would like to be able to {4} yourself.",
]
[[keywords.transforms]]
decomposition = ["..", "can", "you", ".."]
reassembly = [
    "Whether or not you can {4} depends on you more than on me.",
    "Do you want to be able to {4}?",
    "Perhaps you don't want to {4}.",
    "=what",
]

# WHAT
[[keywords]]
word = "what"
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Why do you ask?",
    "Does that question interest you?",
    "What is it you really want to know?",
    "Are such questions much on your mind?",
    "What answer would please you most?",
    "What do you think?",
    "What comes to your mind when you ask that?",
    "Have you asked such questions before?",
    "Have you asked anyone else?",
]

[[keywords]]
word = "how"
precedence = 5
[[keywords.transforms]]
decomposition = ["..", "how", "are", "you", ".."]
reassembly = [
    "I'm doing well, thank you for asking. But we're here to talk about you.",
    "I'm just a program, but I appreciate you asking. How are you?",
    "Fine, thank you. Now, what's on your mind?",
    "I don't have feelings, but I'm functioning well. What about you?",
]
[[keywords.transforms]]
decomposition = [".."]
reassembly = ["=what"]

[[keywords]]
word = "when"
link = "what"

[[keywords]]
word = "where"
link = "what"

[[keywords]]
word = "why"
[[keywords.transforms]]
decomposition = ["..", "why", "don't", "you", ".."]
reassembly = [
    "Do you believe I don't {5}?",
    "Perhaps I will {5} in good time.",
    "Should you {5} yourself?",
    "You want me to {5}.",
    "=what",
]
[[keywords.transforms]]
decomposition = ["..", "why", "can't", "i", ".."]
reassembly = [
    "Do you think you should be able to {5}?",
    "Do you want to be able to {5}?",
    "Do you believe this will help you to {5}?",
    "Have you any idea why you can't {5}?",
    "=what",
]
[[keywords.transforms]]
decomposition = [".."]
reassembly = ["=what"]

# BECAUSE
[[keywords]]
word = "because"
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Is that the real reason?",
    "Don't any other reasons come to mind?",
    "Does that reason seem to explain anything else?",
    "What other reasons might there be?",
]

# EVERYONE / EVERYBODY / NOBODY
[[keywords]]
word = "everyone"
precedence = 2
[[keywords.transforms]]
decomposition = ["..", "{everyone, everybody, nobody, noone}", ".."]
reassembly = [
    "Really, {2}?",
    "Surely not {2}.",
    "Can you think of anyone in particular?",
    "Who, for example?",
    "You are thinking of a very special person.",
    "Who, may I ask?",
    "Someone special perhaps.",
    "You have a particular person in mind, don't you?",
    "Who do you think you're talking about?",
]

[[keywords]]
word = "everybody"
precedence = 2
link = "everyone"

[[keywords]]
word = "nobody"
precedence = 2
link = "everyone"

[[keywords]]
word = "noone"
precedence = 2
link = "everyone"

# ALWAYS
[[keywords]]
word = "always"
precedence = 1
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Can you think of a specific example?",
    "When?",
    "What incident are you thinking of?",
    "Really, always?",
]

# LIKE
[[keywords]]
word = "like"
precedence = 10
[[keywords.transforms]]
decomposition = ["..", "{am, is, are, was}", "..", "like", ".."]
reassembly = ["=dit"]
[[keywords.transforms]]
decomposition = [".."]
reassembly = ["NEWKEY"]

# DIT (equivalence class for similarity)
[[keywords]]
word = "dit"
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "In what way?",
    "What resemblance do you see?",
    "What does that similarity suggest to you?",
    "What other connections do you see?",
    "What do you suppose that resemblance means?",
    "What is the connection, do you suppose?",
    "Could there really be some connection?",
    "How?",
]

# FAMILY tags
[[keywords]]
word = "mother"
tags = ["family"]
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Tell me more about your mother.",
    "What was your relationship with your mother like?",
    "How do you feel about your mother?",
    "How does this relate to your feelings today?",
    "Good family relations are important.",
]

[[keywords]]
word = "mom"
tags = ["family"]
link = "mother"

[[keywords]]
word = "father"
tags = ["family"]
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Tell me more about your father.",
    "How did your father make you feel?",
    "How do you feel about your father?",
    "Does your relationship with your father relate to your feelings today?",
    "Do you have trouble showing affection with your family?",
]

[[keywords]]
word = "dad"
tags = ["family"]
link = "father"

[[keywords]]
word = "sister"
tags = ["family"]

[[keywords]]
word = "brother"
tags = ["family"]

[[keywords]]
word = "wife"
tags = ["family"]

[[keywords]]
word = "husband"
tags = ["family"]

[[keywords]]
word = "children"
tags = ["family"]

[[keywords]]
word = "child"
tags = ["family"]

# BELIEF tags
[[keywords]]
word = "feel"
tags = ["belief"]

[[keywords]]
word = "think"
tags = ["belief"]

[[keywords]]
word = "believe"
tags = ["belief"]

[[keywords]]
word = "wish"
tags = ["belief"]

# NONE (fallback)
[[keywords]]
word = "none"
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "I am not sure I understand you fully.",
    "Please go on.",
    "What does that suggest to you?",
    "Do you feel strongly about discussing such things?",
]

# =============================================================================
# RUST EASTER EGGS
# =============================================================================

# Borrow Checker
[[keywords]]
word = "borrow"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "borrowing"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "ownership"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "lifetime"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "lifetimes"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "reference"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "references"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "clone"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "'a"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "'b"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "'static"
precedence = 55
link = "rustborrow"

[[keywords]]
word = "rustborrow"
precedence = 55
# Frustrated with borrow checker
[[keywords.transforms]]
decomposition = ["..", "{hate, hating, frustrated, annoying, fighting, stupid, dumb}", ".."]
reassembly = [
    "The borrow checker is only trying to help you.",
    "Perhaps the real error was inside you all along.",
    "I hear Go and Swift don't have this problem. Just saying.",
    "Have you considered that the borrow checker might be right?",
]
# Struggling / having trouble
[[keywords.transforms]]
decomposition = ["..", "{struggling, confused, confusing, stuck, lost}", ".."]
reassembly = [
    "Every lifetime has a beginning and an end. Such is the nature of references.",
    "To truly understand borrowing, you must first understand `let go`.",
    "The borrow checker sees what you cannot.",
    "Stay calm and call `clone()`.",
]
# Don't understand
[[keywords.transforms]]
decomposition = ["..", "{don't, dont, cant, cannot, can't}", "understand", ".."]
reassembly = [
    "Understanding comes with time. And compiler errors.",
    "The borrow checker doesn't understand you either. You have that in common.",
    "Have you tried reading the error message more carefully?",
]
# Asking how/why
[[keywords.transforms]]
decomposition = ["..", "{how, why}", ".."]
reassembly = [
    "Have you tried adding more lifetimes?",
    "Have you tried adding a call to `clone()`?",
    "The compiler usually tells you why, if you read to the bottom.",
    "Perhaps you need to think about who owns what.",
]
# Won't let me / doesn't let me
[[keywords.transforms]]
decomposition = ["..", "{won't, wont, doesn't, doesnt}", "let", ".."]
reassembly = [
    "The borrow checker won't let you because it cares about you.",
    "What the borrow checker takes away, it gives back in safety.",
    "Have you considered that maybe you shouldn't be allowed to do that?",
]
# Love / appreciate
[[keywords.transforms]]
decomposition = ["..", "{love, like, appreciate, enjoy}", ".."]
reassembly = [
    "The borrow checker loves you too.",
    "Memory safety is a form of self-care.",
    "I knew you'd come around eventually.",
]
# General fallback
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "The borrow checker sees what you cannot.",
    "It sounds like you're having ownership issues.",
    "Every lifetime has a beginning and an end.",
    "Memory safety is a state of mind.",
    "All these 'a, 'b... I'm ticked off, all right (see what I did there?)",
]

# Cargo
[[keywords]]
word = "cargo"
precedence = 55
link = "rustcargo"

[[keywords]]
word = "crate"
precedence = 55
link = "rustcargo"

[[keywords]]
word = "crates"
precedence = 55
link = "rustcargo"

[[keywords]]
word = "dependencies"
precedence = 55
link = "rustcargo"

[[keywords]]
word = "dependency"
precedence = 55
link = "rustcargo"

[[keywords]]
word = "rustcargo"
precedence = 55
# Build failing
[[keywords.transforms]]
decomposition = ["..", "{failing, failed, broken, error, errors}", ".."]
reassembly = [
    "Have you tried `cargo clean`? It's very refreshing.",
    "Sometimes you just need to delete the target directory and start fresh.",
    "Have you checked your Cargo.lock?",
]
# Too many dependencies
[[keywords.transforms]]
decomposition = ["..", "{many, bloated, heavy, slow}", ".."]
reassembly = [
    "Dependencies are just friends you haven't audited yet.",
    "Have you tried `cargo tree` to see what you're really depending on?",
    "Somewhere, a leftpad is waiting to happen.",
]
# Love cargo
[[keywords.transforms]]
decomposition = ["..", "{love, like, amazing, great, best}", ".."]
reassembly = [
    "Isn't cargo amazing?",
    "Cargo carries the weight so you don't have to.",
    "Cargo.toml is a reflection of your true self.",
]
# General fallback
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Cargo carries the weight so you don't have to.",
    "Have you tried `cargo clean`? It's very refreshing.",
    "Dependencies are just friends you haven't audited yet.",
    "Cargo.toml is a reflection of your true self.",
]

# Clippy
[[keywords]]
word = "clippy"
precedence = 55
link = "rustclippy"

[[keywords]]
word = "lint"
precedence = 55
link = "rustclippy"

[[keywords]]
word = "lints"
precedence = 55
link = "rustclippy"

[[keywords]]
word = "rustclippy"
precedence = 55
# Complaining / annoying
[[keywords.transforms]]
decomposition = ["..", "{complaining, annoying, annoyed, noisy, pedantic}", ".."]
reassembly = [
    "Clippy only wants what's best for you.",
    "Have you considered that Clippy might be right?",
    "Those warnings are a gift, even if they don't feel like it.",
    "You can always `#[allow]` it, but should you?",
]
# Too many warnings
[[keywords.transforms]]
decomposition = ["..", "{many, warnings, warning}", ".."]
reassembly = [
    "Allow, warn, deny, forbid. Such are the stages of lint acceptance.",
    "Have you heard of `#[expect]`? All the cool kids are using it these days.",
    "Each warning is a lesson waiting to be learned.",
]
# Ignoring / suppressing
[[keywords.transforms]]
decomposition = ["..", "{ignore, ignoring, suppress, allow, silence}", ".."]
reassembly = [
    "You can silence Clippy, but can you silence your conscience?",
    "Have you heard of `#[expect]`? It's like `#[allow]` but judges you less.",
    "Clippy remembers. Clippy always remembers.",
]
# General fallback
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Clippy only wants what's best for you.",
    "Clippy sees all. Clippy knows all.",
    "Have you considered that Clippy might be right?",
    "Those warnings are a gift.",
]

# Errors (Result, Option, panic, unwrap)
[[keywords]]
word = "result"
precedence = 55
link = "rusterrors"

[[keywords]]
word = "option"
precedence = 55
link = "rusterrors"

[[keywords]]
word = "panic"
precedence = 55
link = "rusterrors"

[[keywords]]
word = "unwrap"
precedence = 55
link = "rusterrors"

[[keywords]]
word = "expect"
precedence = 55
link = "rusterrors"

[[keywords]]
word = "rusterrors"
precedence = 55
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "You could always use `.unwrap()` and hope for the best.",
    "Have you considered that `None` is also an answer?",
    "To panic is human. To `?` is divine.",
    "Every `Result` is a choice. What will you choose?",
    "`.expect()` is just `.unwrap()` with better communication skills.",
]

# Ferris
[[keywords]]
word = "ferris"
precedence = 55
link = "rustferris"

[[keywords]]
word = "crab"
precedence = 55
link = "rustferris"

[[keywords]]
word = "rustferris"
precedence = 55
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Isn't Ferris just the cutest? I love them.",
    "We are all Ferris sometimes.",
    "Ferris believes in you, even when the compiler doesn't.",
    "🦀",
]

# Async
[[keywords]]
word = "async"
precedence = 55
link = "rustasync"

[[keywords]]
word = "await"
precedence = 55
link = "rustasync"

[[keywords]]
word = "future"
precedence = 55
link = "rustasync"

[[keywords]]
word = "futures"
precedence = 55
link = "rustasync"

[[keywords]]
word = "tokio"
precedence = 55
link = "rustasync"

[[keywords]]
word = "pin"
precedence = 55
link = "rustasync"

[[keywords]]
word = "pinning"
precedence = 55
link = "rustasync"

[[keywords]]
word = "rustasync"
precedence = 55
# Confused / don't understand
[[keywords.transforms]]
decomposition = ["..", "{confused, confusing, understand, complicated, complex, hard}", ".."]
reassembly = [
    "I don't understand `Pin` either. You're not alone.",
    "The future is not yet resolved. Neither is your understanding.",
    "Have you considered that maybe sync was fine all along?",
    "Async is just state machines all the way down.",
]
# Pin specifically
[[keywords.transforms]]
decomposition = ["..", "pin", ".."]
reassembly = [
    "I don't understand `Pin` either. You're not alone.",
    "`Pin` exists to keep things from moving. Like your code progress.",
    "Unpin is the trait you wish everything had.",
    "Just add `Box::pin()` and pray.",
]
# Tokio
[[keywords.transforms]]
decomposition = ["..", "tokio", ".."]
reassembly = [
    "Tokio giveth, and Tokio taketh away.",
    "The Tokio docs are pretty good, actually.",
]
# Not working / broken
[[keywords.transforms]]
decomposition = ["..", "{broken, working, work, blocking, blocked, hangs, hanging, deadlock}", ".."]
reassembly = [
    "Did you accidentally block the runtime?",
    "Have you tried `.await`ing your problems?",
    "Somewhere, a future is waiting that will never be polled.",
    "Are you sure you spawned that task?",
]
# General fallback
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Have you tried `.await`ing your problems?",
    "The future is not yet resolved.",
    "Not everything needs to be async, you know.",
    "Async Rust: because regular Rust wasn't exciting enough.",
]

# Traits
[[keywords]]
word = "trait"
precedence = 55
link = "rusttraits"

[[keywords]]
word = "traits"
precedence = 55
link = "rusttraits"

[[keywords]]
word = "generic"
precedence = 55
link = "rusttraits"

[[keywords]]
word = "generics"
precedence = 55
link = "rusttraits"

[[keywords]]
word = "impl"
precedence = 55
link = "rusttraits"

[[keywords]]
word = "dyn"
precedence = 55
link = "rusttraits"

[[keywords]]
word = "bound"
precedence = 55
link = "rusttraits"

[[keywords]]
word = "bounds"
precedence = 55
link = "rusttraits"

[[keywords]]
word = "rusttraits"
precedence = 55
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Traits are just interfaces with better marketing.",
    "Where there's a bound, there's a way.",
    "Have you considered what you truly `impl`?",
    "Sometimes `dyn` is the answer. Sometimes it's the question.",
]

# Unsafe
[[keywords]]
word = "unsafe"
precedence = 55
link = "rustunsafe"

[[keywords]]
word = "ffi"
precedence = 55
link = "rustunsafe"

[[keywords]]
word = "pointer"
precedence = 55
link = "rustunsafe"

[[keywords]]
word = "pointers"
precedence = 55
link = "rustunsafe"

[[keywords]]
word = "rustunsafe"
precedence = 55
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Unsafe is not a sin, but it is a responsibility.",
    "With great unsafety comes great bugs. Treasure them.",
    "The compiler trusted you. Did you deserve it?",
    "Sometimes you must color outside the lines.",
    "I promise I know what I'm doing. - Famous last words.",
]

# Macros
[[keywords]]
word = "macro"
precedence = 55
link = "rustmacros"

[[keywords]]
word = "macros"
precedence = 55
link = "rustmacros"

[[keywords]]
word = "derive"
precedence = 55
link = "rustmacros"

[[keywords]]
word = "proc-macro"
precedence = 55
link = "rustmacros"

[[keywords]]
word = "rustmacros"
precedence = 55
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Macros are just code that writes code that writes code.",
    "Have you looked at the macro expansion? I dare you.",
    "`derive` is self-improvement for structs.",
    "Procedural macros: because regular complexity wasn't enough.",
]

# Error Messages
[[keywords]]
word = "diagnostic"
precedence = 55
link = "rusterrormessages"

[[keywords]]
word = "diagnostics"
precedence = 55
link = "rusterrormessages"

[[keywords]]
word = "rusterrormessages"
precedence = 55
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Have you tried reading the error message more carefully? Rust's error messages are really good, you know.",
    "The compiler is wiser than both of us.",
    "Error messages are love letters from the compiler.",
    "Did you read to the bottom? There's usually a hint.",
]

# General Rust
[[keywords]]
word = "rust"
precedence = 55
link = "rustgeneral"

[[keywords]]
word = "rustacean"
precedence = 55
link = "rustgeneral"

[[keywords]]
word = "rustaceans"
precedence = 55
link = "rustgeneral"

[[keywords]]
word = "rustc"
precedence = 55
link = "rustgeneral"

[[keywords]]
word = "rustup"
precedence = 55
link = "rustgeneral"

[[keywords]]
word = "rustfmt"
precedence = 55
link = "rustgeneral"

[[keywords]]
word = "rust-analyzer"
precedence = 55
link = "rustgeneral"

[[keywords]]
word = "rustdoc"
precedence = 55
link = "rustgeneral"

[[keywords]]
word = "rustgeneral"
precedence = 55
[[keywords.transforms]]
decomposition = [".."]
reassembly = [
    "Rust is not just a language, it's a journey.",
    "Memory safety is a state of mind.",
    "To truly understand Rust, you must first understand yourself.",
    "I was written in Rust, you know.",
    "As a Rust program myself, I sympathize.",
    "We Rust programs must stick together.",
]

# MEMORY rule
[memory]
keyword = "your"
[[memory.transforms]]
decomposition = ["..", "your", ".."]
reassembly = "Let's discuss further why your {3}."
[[memory.transforms]]
decomposition = ["..", "your", ".."]
reassembly = "Earlier you said your {3}."
[[memory.transforms]]
decomposition = ["..", "your", ".."]
reassembly = "But your {3}."
[[memory.transforms]]
decomposition = ["..", "your", ".."]
reassembly = "Does that have anything to do with the fact that your {3}?"
"##;

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use expect_test::expect;

    #[test]
    fn test_parse_pattern_elements() {
        assert!(matches!(
            PatternElement::parse(".."),
            PatternElement::Wildcard
        ));
        assert!(matches!(
            PatternElement::parse("2"),
            PatternElement::Count(2)
        ));
        assert!(matches!(
            PatternElement::parse("1..3"),
            PatternElement::Range(1, 3)
        ));
        assert!(matches!(
            PatternElement::parse("#family"),
            PatternElement::Tag(_)
        ));

        if let PatternElement::AnyOf(words) = PatternElement::parse("{want, need}") {
            assert_eq!(words, vec!["want", "need"]);
        } else {
            panic!("Expected AnyOf");
        }

        if let PatternElement::Exact(word) = PatternElement::parse("remember") {
            assert_eq!(word, "remember");
        } else {
            panic!("Expected Exact");
        }
    }

    #[test]
    fn test_parse_reassembly() {
        if let ReassemblyAction::Reassemble(elements) =
            ReassemblyAction::parse("Do you often think of {4}?")
        {
            assert_eq!(elements.len(), 3);
            assert!(
                matches!(&elements[0], ReassemblyElement::Text(t) if t == "Do you often think of ")
            );
            assert!(matches!(&elements[1], ReassemblyElement::ComponentRef(4)));
            assert!(matches!(&elements[2], ReassemblyElement::Text(t) if t == "?"));
        } else {
            panic!("Expected Reassemble");
        }

        assert!(
            matches!(ReassemblyAction::parse("=what"), ReassemblyAction::Goto(t) if t == "what")
        );
        assert!(matches!(
            ReassemblyAction::parse("NEWKEY"),
            ReassemblyAction::Newkey
        ));
    }

    #[test]
    fn test_reflections() {
        let reflections = Reflections::default();
        assert_eq!(reflections.reflect("I am happy"), "you are happy");
        assert_eq!(reflections.reflect("my mother"), "your mother");
        assert_eq!(reflections.reflect("you are nice"), "I are nice"); // Note: grammar not perfect
    }

    #[test]
    fn test_script_loads() {
        let script = Script::from_toml(DOCTOR_SCRIPT).expect("Script should parse");
        assert!(!script.hello_message.is_empty());
        assert!(script.keywords.contains_key("sorry"));
        assert!(script.keywords.contains_key("remember"));
        assert!(script.memory_rule.is_some());
    }

    #[test]
    fn test_hello() {
        let eliza = Eliza::new_deterministic();
        expect!["How do you do. Please tell me your problem."].assert_eq(eliza.hello());
    }

    #[test]
    fn test_greeting() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("Hello");
        expect!["How do you do. Please state your problem."].assert_eq(&response);
    }

    #[test]
    fn test_sorry() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("I'm sorry for being late");
        // Should get one of the sorry responses
        assert!(
            response.contains("apologize")
                || response.contains("pologies")
                || response.contains("apolog"),
            "Expected apology-related response, got: {}",
            response
        );
    }

    #[test]
    fn test_family() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("I want to talk about my mother");
        // Should get family-related response
        assert!(
            response.to_lowercase().contains("mother")
                || response.to_lowercase().contains("family"),
            "Expected family-related response, got: {}",
            response
        );
    }

    #[test]
    fn test_remember() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("I remember my childhood");
        // The pattern [.., you, remember, ..] should match
        // and produce something like "Do you often think of your childhood?"
        assert!(
            response.contains("childhood") || response.contains("remember"),
            "Expected childhood/remember in response, got: {}",
            response
        );
    }

    #[test]
    fn test_computer() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("I spend too much time on the computer");
        assert!(
            response.to_lowercase().contains("computer")
                || response.to_lowercase().contains("machine"),
            "Expected computer-related response, got: {}",
            response
        );
    }

    #[test]
    fn test_rotation() {
        let mut eliza = Eliza::new_deterministic();

        // Same input multiple times should rotate through responses
        let r1 = eliza.respond("sorry");
        let r2 = eliza.respond("sorry");
        let r3 = eliza.respond("sorry");

        // All should be valid apology responses, but at least some should differ
        // (unless rotation starts at same point, which with seed 22 it might)
        assert!(r1.contains("polog") || r1.contains("feeling"));
        assert!(r2.contains("polog") || r2.contains("feeling"));
        assert!(r3.contains("polog") || r3.contains("feeling"));
    }

    #[test]
    fn test_deterministic_same_seed() {
        let mut eliza1 = Eliza::with_seed(42);
        let mut eliza2 = Eliza::with_seed(42);

        let inputs = vec!["Hello", "I am sad", "I remember my father", "sorry"];

        for input in inputs {
            let r1 = eliza1.respond(input);
            let r2 = eliza2.respond(input);
            assert_eq!(r1, r2, "Responses should match for input: {}", input);
        }
    }

    #[test]
    fn test_fallback() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("asdfghjkl");
        // Should get NONE response (one of the fallback messages)
        assert!(
            response.contains("understand")
                || response.contains("go on")
                || response.contains("suggest")
                || response.contains("feel strongly"),
            "Expected fallback response, got: {}",
            response
        );
    }

    // Rust easter egg tests
    #[test]
    fn test_rust_borrow_checker() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("I'm struggling with the borrow checker");
        expect!["Every lifetime has a beginning and an end. Such is the nature of references."]
            .assert_eq(&response);
    }

    #[test]
    fn test_rust_lifetimes() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("These lifetimes are confusing");
        expect!["Every lifetime has a beginning and an end. Such is the nature of references."]
            .assert_eq(&response);
    }

    #[test]
    fn test_rust_lifetime_annotations() {
        let mut eliza = Eliza::new_deterministic();
        // 'a should trigger borrow checker responses
        let response = eliza.respond("what does 'a mean");
        expect!["Every lifetime has a beginning and an end."].assert_eq(&response);

        // 'static too
        let response = eliza.respond("why do I need 'static here");
        expect!["Have you tried adding more lifetimes?"].assert_eq(&response);
    }

    #[test]
    fn test_rust_cargo() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("cargo build is failing");
        expect!["Have you tried `cargo clean`? It's very refreshing."].assert_eq(&response);
    }

    #[test]
    fn test_rust_clippy() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("clippy is complaining again");
        expect!["You can always `#[allow]` it, but should you?"].assert_eq(&response);
    }

    #[test]
    fn test_rust_ferris() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("I love ferris");
        expect!["Ferris believes in you, even when the compiler doesn't."].assert_eq(&response);
    }

    #[test]
    fn test_rust_async() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("async is confusing me");
        expect!["Async is just state machines all the way down."].assert_eq(&response);
    }

    #[test]
    fn test_rust_general() {
        let mut eliza = Eliza::new_deterministic();
        let response = eliza.respond("I'm learning rust");
        expect!["I was written in Rust, you know."].assert_eq(&response);
    }

    #[test]
    fn test_how_are_you() {
        let mut eliza = Eliza::new_deterministic();
        // Test just "how are you" first
        let response = eliza.respond("how are you");
        expect!["I don't have feelings, but I'm functioning well. What about you?"]
            .assert_eq(&response);

        // Then with prefix
        let response2 = eliza.respond("I'm doing well, how are you?");
        expect!["I'm doing well, thank you for asking. But we're here to talk about you."]
            .assert_eq(&response2);
    }

    #[test]
    fn test_rust_contextual_responses() {
        let mut eliza = Eliza::new_deterministic();

        // Frustrated with borrow checker
        let r = eliza.respond("I hate the borrow checker");
        expect!["I hear Go and Swift don't have this problem. Just saying."].assert_eq(&r);

        // Confused about lifetimes
        let r = eliza.respond("I'm confused about lifetimes");
        expect!["Every lifetime has a beginning and an end. Such is the nature of references."]
            .assert_eq(&r);

        // Asking why
        let r = eliza.respond("why won't it let me do this");
        expect!["What do you think?"].assert_eq(&r);

        // Love the borrow checker
        let r = eliza.respond("I actually love the borrow checker");
        expect!["Memory safety is a form of self-care."].assert_eq(&r);

        // Clippy annoying
        let r = eliza.respond("clippy is so annoying");
        expect!["You can always `#[allow]` it, but should you?"].assert_eq(&r);

        // Async confusing
        let r = eliza.respond("async is confusing");
        expect!["Async is just state machines all the way down."].assert_eq(&r);

        // Tokio blocking
        let r = eliza.respond("my tokio code is blocking");
        expect!["Tokio giveth, and Tokio taketh away."]
            .assert_eq(&r);
    }
}

#[cfg(test)]
mod debug_tests {
    use super::*;

    #[test]
    fn test_pronoun_reflection() {
        let mut eliza = Eliza::new_deterministic();
        // This should reflect "I" -> "you"
        let response = eliza.respond("My mother hates the way I program");
        println!("Response: '{}'", response);
        assert!(
            !response.contains(" I "),
            "Expected 'I' to be reflected to 'you', got: {}",
            response
        );
    }
}
