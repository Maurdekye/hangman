#![feature(iterator_try_collect)]
#![feature(file_create_new)]
use std::{
    collections::{hash_map::Entry, HashMap}, error::Error, fs::File, io::{stdin, stdout, BufRead, BufReader, BufWriter, Write}, ops::ControlFlow, path::PathBuf
};

use clap::{value_parser, Parser};
use regex::Regex;
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

struct HistoryFrame {
    guess: Vec<Option<char>>,
    not_present: Vec<char>,
}

struct Game {
    guess_pattern: Regex,
    words: Vec<String>,
    available_words: Vec<String>,
    current_guess: Vec<Option<char>>,
    not_present: Vec<char>,
    guess_history: Vec<HistoryFrame>,
    args: Args,
}

impl Game {
    pub fn new(args: Args, words: Vec<String>) -> Result<Game, Err> {

        let letters = args.letters as usize;
        let words: Vec<String> = words
            .into_iter()
            .filter(|word| word.len() == letters)
            .collect();
        Ok(Game {
            guess_pattern: Regex::new(r"^([a-z])(( [0-9]+)*)$")?,
            available_words: words.clone(),
            words,
            current_guess: vec![None; letters],
            not_present: vec![],
            guess_history: vec![],
            args
        })
    }

    fn print_stats(&self) {
        println!(
            "current guess: {}",
            self
                .current_guess
                .iter()
                .map(|letter| match letter {
                    None => "_".to_string(),
                    Some(letter) => (*letter).into(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        );
        if !self.not_present.is_empty() {
            println!(
                "letters not present: {}",
                self
                    .not_present
                    .iter()
                    .cloned()
                    .map(String::from)
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
        println!("{} possible words", self.available_words.len());
    }

    fn compute_letter_scores(&self, used: &[char]) -> Vec<(char, usize)> {
        let mut counts: HashMap<_, _> = ('a'..='z')
            .filter(|l| !used.contains(&l))
            .map(|l| (l, 0usize))
            .collect();
        for word in self.available_words.iter() {
            let mut unique_letters: Vec<_> = word.chars().collect();
            unique_letters.sort_unstable();
            unique_letters.dedup();
            for letter in unique_letters {
                if let Entry::Occupied(mut entry) = counts.entry(letter) {
                    *entry.get_mut() += 1;
                }
            }
        }
        let mut sorted_counts: Vec<_> = counts.into_iter().collect();
        sorted_counts.sort_unstable_by(|(_, a), (_, b)| b.cmp(a));

        return sorted_counts;
    }

    fn show_scores_or_guesses(&self) -> Vec<char> {
        let used: Vec<_> = self
            .current_guess
            .iter()
            .filter_map(|l| l.as_ref())
            .chain(self.not_present.iter())
            .cloned()
            .collect();

        if self.available_words.len() <= self.args.display_guesses_threshold as usize {
            println!("Possibilities:");

            for word in self.available_words.iter() {
                println!("{word}");
            }
        }

        let letter_scores = self.compute_letter_scores(&used);
        
        println!("Top {} guesses:", self.args.num_suggestions);
        for (i, (letter, score)) in letter_scores
            .into_iter()
            .take(self.args.num_suggestions as usize)
            .enumerate()
        {
            println!("{}. {letter}: {score}", i + 1);
        }

        used
    }

    fn read_guess(&mut self, used: &[char]) -> Result<ControlFlow<(char, Vec<usize>), HistoryFrame>, Err> {
        let letters = self.args.letters as usize;
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
                let Some(last_frame) = self.guess_history.pop() else {
                    println!("Nothing to undo!");
                    continue;
                };

                return Ok(Continue(last_frame));
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

            if positions.iter().any(|&p| p == 0 || p > letters) {
                println!("Positions provided are invalid letter indicies");
                continue;
            }

            let positions: Vec<_> = positions.into_iter().map(|p| p - 1).collect();

            if let Some(pos) = positions.iter().find(|&&pos| self.current_guess[pos].is_some()) {
                println!("Letter {} is already occupied", pos + 1);
                continue;
            }

            return Ok(Break((letter, positions)));
        };
    }

    fn mark_result(&mut self, letter: char, positions: Vec<usize>) {
        self.guess_history.push(HistoryFrame {
            guess: self.current_guess.clone(),
            not_present: self.not_present.clone()
        });

        if positions.is_empty() {
            println!("Letter {letter} is not in the word");
            self.not_present.push(letter);
        } else {
            println!(
                "Letter {letter} is at position(s) {} of the word",
                positions
                    .iter()
                    .map(|p| (p + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for pos in positions {
                self.current_guess[pos] = Some(letter);
            }
        }
    }

    fn prune_words(&mut self) -> Vec<Vec<char>> {
        let mut potential_letters = vec![vec![]; self.args.letters as usize];
        
        self.available_words.retain(|word| {
            let mut potential_additions = vec![vec![]; self.args.letters as usize];
            for ((potential_place_additions, potential_place_letters), (word_letter, guess_letter)) in (potential_additions
                .iter_mut().zip(potential_letters.iter()))
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
                    None if potential_place_letters.len() < 26 => potential_place_additions.push(word_letter),
                    _ => {}
                }
            }
            for (potential_place_additions, potential_place_letters) in potential_additions.into_iter().zip(potential_letters.iter_mut()) {
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
        for (guess_letter, potential_letter) in
            self.current_guess.iter_mut().zip(potential_letters)
        {
            if let (None, &[letter]) = (&guess_letter, &potential_letter[..]) {
                *guess_letter = Some(letter);
            }
        }
    }

    pub fn play(&mut self) -> Result<String, Err> {
        loop {
            self.print_stats();
            
            println!();
    
            let used = self.show_scores_or_guesses();
    
            println!();
    

            match self.read_guess(&used)? {
                Break((letter, positions)) => {
                    self.mark_result(letter, positions);
                }
                Continue(frame) => {
                    self.current_guess = frame.guess;
                    self.not_present = frame.not_present;
                    self.available_words = self.words.clone();
                }
            }

            let potential_letters = self.prune_words();
            self.fill_certain_letters(potential_letters);
    
            // check if there's only one or zero guesses left
            match &self.available_words[..] {
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

#[derive(Parser)]
struct Args {
    #[clap(
        value_parser = value_parser!(u32).range(1..),
        help = "Number of letters in the word being guessed", 
    )]
    letters: u32,

    #[clap(
        short = 'f',
        long,
        default_value = "./words.txt",
        help = "Name of the file to cache and load words from"
    )]
    words_file: PathBuf,

    #[clap(
        short = 's',
        long,
        default_value = "https://www.mit.edu/~ecprice/wordlist.100000",
        help = "Url to load words from if not downloaded"
    )]
    word_source: String,

    #[clap(
        short, 
        long, 
        default_value_t = 5, 
        value_parser = value_parser!(u32).range(1..),
        help = "Number of top letter suggestions to display", 
    )]
    num_suggestions: u32,

    #[clap(
        short,
        long,
        default_value_t = 10,
        value_parser = value_parser!(u32).range(1..),
        help = "Show possible words to guess once the total number of possible words goes below this threshold"
    )]
    display_guesses_threshold: u32,
}

fn main() -> Result<(), Err> {
    let args = Args::parse();
    let words = load_words(&args)?;
    println!("Loaded {} words", words.len());
    let mut game = Game::new(args, words)?;
    let final_guess = game.play()?;
    println!("Final guess: {final_guess}");
    Ok(())
}
