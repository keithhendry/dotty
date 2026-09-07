use rand::{distributions::Alphanumeric, Rng};

pub fn random_string(size: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(size)
        .map(char::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_requested_length() {
        assert_eq!(random_string(10).len(), 10);
        assert_eq!(random_string(0).len(), 0);
    }

    #[test]
    fn returns_only_alphanumeric_chars() {
        let s = random_string(200);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn is_not_deterministic() {
        assert_ne!(random_string(32), random_string(32));
    }
}
