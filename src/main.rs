#![feature(iterator_try_collect)]
#![feature(file_create_new)]
use std::{
    collections::{hash_map::Entry, HashMap},
    error::Error,
    fs::File,
    io::{stdin, stdout, BufRead, BufReader, BufWriter, Write},
    num::ParseIntError,
    ops::ControlFlow,
    panic::catch_unwind,
    path::PathBuf,
    time::Duration,
};

use clap::{ArgAction, Parser, Subcommand};
use progress_observer::{reprint, Observer};
use regex::Regex;
use serde::Serialize;
use ControlFlow::*;

type Err = Box<dyn Error>;

fn load_words(args: &Args) -> Result<Vec<String>, Err> {
    if let Ok(words_file) = File::open(&args.words_file) {
        println!("Loading from {:?}", &args.words_file);
        Ok(BufReader::new(words_file).lines().try_collect()?)
    } else {
        println!(
            "Downloading words from {} and saving to {:?}",
            &args.word_source, &args.words_file
        );
        let mut words_file = BufWriter::new(File::create_new(&args.words_file)?);
        Ok(BufReader::new(reqwest::blocking::get(&args.word_source)?)
            .lines()
            .map(|line| {
                let line = line?;
                words_file.write(line.as_bytes())?;
                words_file.write(b"\n")?;
                Ok::<_, Err>(line)
            })
            .try_collect()?)
    }
}

struct HangmanPlayer {
    available_words: Vec<String>,
    current_guess: Vec<Option<char>>,
    not_present: Vec<char>,
    used_letters: Vec<char>,
    guess_history: Vec<HistoryFrame>,
}

impl std::fmt::Debug for HangmanPlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HangmanPlayer")
            .field("available_words.len()", &self.available_words.len())
            .field("current_guess", &self.current_guess)
            .field("not_present", &self.not_present)
            // .field("guess_history", &self.guess_history)
            .finish()
    }
}

impl HangmanPlayer {
    pub fn new(words: Vec<String>, word_length: usize) -> Result<HangmanPlayer, Err> {
        let words: Vec<String> = words
            .into_iter()
            .filter(|word| word.len() == word_length)
            .collect();
        Ok(HangmanPlayer {
            available_words: words.clone(),
            current_guess: vec![None; word_length],
            not_present: vec![],
            used_letters: vec![],
            guess_history: vec![],
        })
    }

    fn compute_letter_scores(&self) -> Vec<(char, usize)> {
        let mut counts: HashMap<_, _> = ('a'..='z')
            .filter(|l| !self.used_letters.contains(&l))
            .map(|l| (l, 0usize))
            .collect();
        for word in self.available_words.iter() {
            let mut unique_letters: Vec<_> = word.chars().collect();
            unique_letters.sort();
            unique_letters.dedup();
            for letter in unique_letters {
                if let Entry::Occupied(mut entry) = counts.entry(letter) {
                    *entry.get_mut() += 1;
                }
            }
        }
        let mut sorted_counts: Vec<_> = counts.into_iter().collect();
        sorted_counts.sort_by(|(_, a), (_, b)| b.cmp(a));

        return sorted_counts;
    }

    fn push_history(&mut self) {
        self.guess_history.push(HistoryFrame {
            guess: self.current_guess.clone(),
            not_present: self.not_present.clone(),
        });
    }

    fn mark_result(&mut self, letter: char, positions: Vec<usize>) {
        self.push_history();

        self.used_letters.push(letter);
        if positions.is_empty() {
            self.not_present.push(letter);
        } else {
            for pos in positions {
                self.current_guess[pos] = Some(letter);
            }
        }
    }

    fn prune_words(&mut self) -> Vec<Vec<char>> {
        let mut potential_letters = vec![vec![]; self.current_guess.len()];

        self.available_words.retain(|word| {
            let mut potential_additions = vec![vec![]; self.current_guess.len()];
            for (
                (potential_place_additions, potential_place_letters),
                (word_letter, guess_letter),
            ) in (potential_additions.iter_mut().zip(potential_letters.iter()))
                .zip(word.chars().zip(self.current_guess.iter()))
            {
                if self.not_present.contains(&word_letter) {
                    return false;
                }
                match guess_letter {
                    Some(placed_letter) => {
                        if placed_letter != &word_letter {
                            return false;
                        }
                    }
                    None if potential_place_letters.len() < 26 => {
                        potential_place_additions.push(word_letter)
                    }
                    _ => {}
                }
            }
            for (potential_place_additions, potential_place_letters) in potential_additions
                .into_iter()
                .zip(potential_letters.iter_mut())
            {
                for letter_addition in potential_place_additions {
                    if !potential_place_letters.contains(&letter_addition) {
                        potential_place_letters.push(letter_addition);
                    }
                }
            }
            true
        });
        potential_letters
    }

    fn fill_certain_letters(&mut self, potential_letters: Vec<Vec<char>>) {
        for (guess_letter, potential_letter) in self.current_guess.iter_mut().zip(potential_letters)
        {
            if let (None, &[letter]) = (&guess_letter, &potential_letter[..]) {
                *guess_letter = Some(letter);
            }
        }
    }

    fn prune_and_fill_certain_letters(&mut self) {
        let potential_letters = self.prune_words();
        self.fill_certain_letters(potential_letters);
    }
}

struct PlayerUI {
    player: HangmanPlayer,
    args: PlayArgs,
    guess_pattern: Regex,
    original_word_list: Vec<String>,
}

impl PlayerUI {
    pub fn new(player: HangmanPlayer, args: PlayArgs) -> PlayerUI {
        PlayerUI {
            original_word_list: player.available_words.clone(),
            player,
            args,
            guess_pattern: Regex::new(r"^([a-z])(( [0-9]+)*)$").unwrap(),
        }
    }

    fn print_stats(&self) {
        println!(
            "current guess: {}",
            self.player
                .current_guess
                .iter()
                .map(|letter| match letter {
                    None => "_".to_string(),
                    Some(letter) => (*letter).into(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        );
        if !self.player.not_present.is_empty() {
            println!(
                "letters not present: {}",
                self.player
                    .not_present
                    .iter()
                    .cloned()
                    .map(String::from)
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
        println!("{} possible words", self.player.available_words.len());
    }

    fn show_scores_guesses_possibilities(&self, letter_scores: &Vec<(char, usize)>) {
        if self.player.available_words.len() <= self.args.display_guesses_threshold {
            println!("Possibilities:");

            for word in self.player.available_words.iter() {
                println!("{word}");
            }
        }

        println!("Top {} guesses:", self.args.num_suggestions);
        for (i, (letter, score)) in letter_scores
            .into_iter()
            .take(self.args.num_suggestions)
            .enumerate()
        {
            println!("{}. {letter}: {score}", i + 1);
        }
    }

    fn read_guess(&self, used: &[char]) -> Result<ControlFlow<(char, Vec<usize>), Undo>, Err> {
        const HELPTEXT: &str = "Type your guess in the following format: <letter> [positions]
example 1: the letter n appears at the start of the word: type `n 1`
example 2: the letter e appears as the second and fourth letter: type `e 2 4`
example 3: the letter g does not appear in the word: type `g`
Type `undo` to undo the last input";
        loop {
            print!("Type the letter you guessed, and if/where it appears in the word (hit enter for help): ");
            stdout().flush()?;
            let mut guess_raw = String::new();
            stdin().read_line(&mut guess_raw)?;
            guess_raw = guess_raw.trim().to_lowercase().to_string();

            if guess_raw.is_empty() {
                println!("{HELPTEXT}");
                continue;
            }

            if guess_raw == "undo" {
                if self.player.guess_history.is_empty() {
                    println!("Nothing to undo!");
                    continue;
                }

                return Ok(Continue(Undo));
            }

            let Some(captures) = self.guess_pattern.captures(&guess_raw) else {
                println!("Invalid guess format");
                println!("{HELPTEXT}");
                continue;
            };

            let letter = captures.get(1).unwrap().as_str().chars().next().unwrap();

            if used.contains(&&letter) {
                println!("{letter} has already been guessed");
                continue;
            }

            let raw_positions = captures.get(2).unwrap();

            if raw_positions.is_empty() {
                return Ok(Break((letter, vec![])));
            }

            let positions: Vec<usize> = raw_positions
                .as_str()
                .trim()
                .split(" ")
                .map(|t| t.parse().unwrap())
                .collect();

            if positions.iter().any(|&p| p == 0 || p > self.args.letters) {
                println!("Positions provided are invalid letter indicies");
                continue;
            }

            let positions: Vec<_> = positions.into_iter().map(|p| p - 1).collect();

            if let Some(pos) = positions
                .iter()
                .find(|&&pos| self.player.current_guess[pos].is_some())
            {
                println!("Letter {} is already occupied", pos + 1);
                continue;
            }

            return Ok(Break((letter, positions)));
        }
    }

    pub fn play(&mut self) -> Result<String, Err> {
        loop {
            self.print_stats();

            println!();

            let letter_scores = self.player.compute_letter_scores();
            self.show_scores_guesses_possibilities(&letter_scores);

            println!();

            match self.read_guess(&self.player.used_letters)? {
                Break((letter, positions)) => {
                    if positions.is_empty() {
                        println!("Letter {letter} is not in the word");
                    } else {
                        println!(
                            "Letter {letter} is at position(s) {} of the word",
                            positions
                                .iter()
                                .map(|p| (p + 1).to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    self.player.mark_result(letter, positions);
                }
                Continue(Undo) => {
                    let frame = self.player.guess_history.pop().unwrap();
                    self.player.current_guess = frame.guess;
                    self.player.not_present = frame.not_present;
                    self.player.available_words = self.original_word_list.clone();
                }
            }

            self.player.prune_and_fill_certain_letters();

            match &self.player.available_words[..] {
                [word] => {
                    return Ok(word.clone());
                }
                [] => {
                    Err("No possible words left! is it in the database / did you make a mistake?")?;
                }
                _ => {}
            }
        }
    }
}

fn simulate(words: Vec<String>, word: String) -> Result<SimResults, Err> {
    let mut player = HangmanPlayer::new(words, word.len())?;
    let mut mistakes = 0;
    let mut guesses = Vec::new();

    loop {
        let scores = player.compute_letter_scores();
        let letter = scores[0].0; // simulate guess
        let positions: Vec<_> = word
            .chars()
            .enumerate()
            .filter_map(|(i, c)| (c == letter).then_some(i))
            .collect(); // simulate receiving the result of the guess
        if positions.is_empty() {
            mistakes += 1;
        }
        guesses.push(letter);
        player.mark_result(letter, positions);
        player.prune_and_fill_certain_letters();
        match &player.available_words[..] {
            [single] if single == &word => {
                player.push_history();
                return Ok(SimResults {
                    history: player.guess_history,
                    guesses,
                    mistakes,
                });
            }
            [] => Err("No words left")?,
            [single] => Err(format!("Final result '{single}' is not the correct word"))?,
            _ => {}
        }
    }
}

struct Undo;

#[derive(Debug)]
struct HistoryFrame {
    guess: Vec<Option<char>>,
    not_present: Vec<char>,
}

struct SimResults {
    history: Vec<HistoryFrame>,
    guesses: Vec<char>,
    mistakes: usize,
}

fn nonzero(arg: &str) -> Result<usize, String> {
    let val: usize = arg.parse().map_err(|e: ParseIntError| e.to_string())?;
    if val == 0 {
        Err("Value must be at least 1!")?;
    }
    Ok(val)
}

#[derive(Parser)]
struct Args {
    /// Name of the file to cache and load words from
    #[clap(short = 'f', long, default_value = "./words.txt")]
    words_file: PathBuf,

    /// Url to load words from if not downloaded
    #[clap(
        short = 's',
        long,
        default_value = "https://www.mit.edu/~ecprice/wordlist.100000"
    )]
    word_source: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Play hangman with someone
    Play(PlayArgs),

    /// Simulate playing hangman with a specific word, and show statistics of the result
    Simulate(SimulateArgs),

    /// Simulate all words in the dictionary, storing the results in a csv file
    BulkSim(BulkSimArgs),
}

#[derive(Parser)]
struct PlayArgs {
    /// Number of letters in the word being guessed
    #[clap(value_parser = nonzero)]
    letters: usize,

    /// Number of top letter suggestions to display
    #[clap(short, long, default_value_t = 5, value_parser = nonzero)]
    num_suggestions: usize,

    /// Show possible words to guess once the total number of possible words goes below this threshold
    #[clap(short, long, default_value_t = 10, value_parser = nonzero)]
    display_guesses_threshold: usize,
}

#[derive(Parser)]
struct SimulateArgs {
    /// Word to simulate
    word: String,

    /// Show detailed simulation results
    #[clap(short, long, action = ArgAction::SetTrue)]
    detailed: bool,
}

#[derive(Parser)]
struct BulkSimArgs {
    /// Output file
    #[clap(short, long, default_value = "scores.csv")]
    out: PathBuf,
}

#[derive(Serialize)]
struct SimRecord(String, usize, usize);

fn main() -> Result<(), Err> {
    let args = Args::parse();
    let words = load_words(&args)?;
    println!("Loaded {} words", words.len());

    match args.command {
        Command::Play(args) => {
            let mut game = PlayerUI::new(HangmanPlayer::new(words, args.letters)?, args);
            let final_guess = game.play()?;
            println!("Final guess: {final_guess}");
        }
        Command::Simulate(args) => {
            let results = simulate(words, args.word)?;
            println!(
                "Took {} guesses to guess the word, making {} total mistakes",
                results.history.len(),
                results.mistakes
            );

            if args.detailed {
                for ((i, frame), guess) in (1..).zip(results.history).zip(results.guesses) {
                    println!(
                        "Turn {i}: {}, [{}], guessed {guess}",
                        frame
                            .guess
                            .iter()
                            .map(|letter| match letter {
                                None => "_".to_string(),
                                Some(letter) => (*letter).into(),
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                        frame
                            .not_present
                            .iter()
                            .cloned()
                            .map(String::from)
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
            }
        }
        Command::BulkSim(args) => {
            let mut writer = csv::WriterBuilder::new().from_path(args.out)?;
            for (i, (word, log)) in words
                .iter()
                .zip(Observer::new(Duration::from_secs_f32(0.1)))
                .enumerate()
            {
                if log {
                    reprint!("{}/{}", i, words.len());
                }
                let SimResults {
                    history, mistakes, ..
                } = catch_unwind(|| simulate(words.clone(), word.clone()))
                    .map_err(|_| println!("Failed on '{word}'"))
                    .unwrap()?;
                let row = SimRecord(word.clone(), history.len(), mistakes);
                writer.serialize(row)?;
            }
        }
    }

    Ok(())
}
