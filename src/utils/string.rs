use rand::{distributions::Alphanumeric, Rng};

pub fn random_string(size: usize) -> String {
    return rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(size)
        .map(char::from)
        .collect();
}
