//! The client side of SCRAM-SHA-256, RFC 5802 and RFC 7677, without channel binding.
//!
//! The harness's own client has no TLS, so it never offers `SCRAM-SHA-256-PLUS`. The channel
//! binding cases run through libpq and the client suites.

use crate::crypto::{base64_decode, base64_encode, hmac_sha256, pbkdf2_sha256, sha256};

#[derive(Debug)]
pub(crate) struct Scram {
    password: String,
    nonce: String,
    client_first_bare: String,
    server_signature: Option<[u8; 32]>,
}

impl Scram {
    /// Starts an exchange. PostgreSQL ignores the user name in the SCRAM messages and takes it
    /// from the startup message, so the harness sends an empty one, as libpq does.
    pub(crate) fn new(password: &str, nonce: &str) -> Scram {
        Scram::with_user("", password, nonce)
    }

    fn with_user(user: &str, password: &str, nonce: &str) -> Scram {
        Scram {
            password: password.to_string(),
            nonce: nonce.to_string(),
            client_first_bare: format!("n={user},r={nonce}"),
            server_signature: None,
        }
    }

    /// The client-first-message, with the GS2 header for "no channel binding".
    pub(crate) fn client_first(&self) -> String {
        format!("n,,{}", self.client_first_bare)
    }

    /// The client-final-message, from the server-first-message.
    pub(crate) fn client_final(&mut self, server_first: &str) -> Result<String, String> {
        let mut nonce = None;
        let mut salt = None;
        let mut iterations = None;
        for part in server_first.split(',') {
            match part.split_once('=') {
                Some(("r", v)) => nonce = Some(v),
                Some(("s", v)) => salt = base64_decode(v),
                Some(("i", v)) => iterations = v.parse::<u32>().ok(),
                _ => {}
            }
        }
        let (Some(nonce), Some(salt), Some(iterations)) = (nonce, salt, iterations) else {
            return Err(format!("a server-first-message without r, s and i: {server_first:?}"));
        };
        if !nonce.starts_with(&self.nonce) {
            return Err("the server nonce does not start with the client nonce".to_string());
        }
        let salted = pbkdf2_sha256(self.password.as_bytes(), &salt, iterations);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);
        let without_proof = format!("c=biws,r={nonce}");
        let auth_message = format!("{},{server_first},{without_proof}", self.client_first_bare);
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let proof: Vec<u8> = client_key.iter().zip(client_signature).map(|(k, s)| k ^ s).collect();
        let server_key = hmac_sha256(&salted, b"Server Key");
        self.server_signature = Some(hmac_sha256(&server_key, auth_message.as_bytes()));
        Ok(format!("{without_proof},p={}", base64_encode(&proof)))
    }

    /// Checks the server-final-message. A server that cannot prove that it knows the password is
    /// not the server the harness meant to reach.
    pub(crate) fn verify(&self, server_final: &str) -> Result<(), String> {
        let expected = self
            .server_signature
            .ok_or("a server-final-message before the client-final-message")?;
        match server_final.strip_prefix("v=").and_then(base64_decode) {
            Some(signature) if signature == expected => Ok(()),
            _ => Err(format!("the server signature does not match: {server_final:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Scram;

    /// The example exchange in RFC 7677 section 3.
    #[test]
    fn the_rfc_7677_exchange() {
        let mut scram = Scram::with_user("user", "pencil", "rOprNGfwEbeRWgbNEkqO");
        assert_eq!(scram.client_first(), "n,,n=user,r=rOprNGfwEbeRWgbNEkqO");
        let server_first = "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        assert_eq!(
            scram.client_final(server_first).unwrap(),
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
        scram.verify("v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=").unwrap();
        assert!(scram.verify("v=AAAA").is_err());
    }
}
