#![feature(iterator_try_collect)]
#![feature(file_create_new)]
use std::{
    error::Error,
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::PathBuf,
};

use clap::Parser;

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

#[derive(Parser)]
struct Args {
    #[clap(
        short = 'f',
        long,
        default_value = "./words.txt",
        help = "Name of the file to cache words in"
    )]
    words_file: PathBuf,

    #[clap(
        short = 's',
        long,
        default_value = "https://www.mit.edu/~ecprice/wordlist.10000",
        help = "Url to fetch words from if not cached"
    )]
    word_source: String,
}

fn main() -> Result<(), Err> {
    let args = Args::parse();
    let words = load_words(&args)?;
    println!("Loaded {} words", words.len());
    Ok(())
}
