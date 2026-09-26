/// `ActiveSupport::KeyGenerator` (PBKDF2, with Rails' configured digest and iterations), with caching.
pub struct KeyGenerator { secret: String }

impl KeyGenerator {
    pub fn new(secret_key_base: &str) -> Self { Self { secret: secret_key_base.to_string() } }
    pub fn generate_key(&self, salt: &str, length: usize) -> Vec<u8> { let _ = (&self.secret, salt, length); todo!() }
}
