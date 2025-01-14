// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::bn254::zk_login::poseidon_zk_login;
use crate::bn254::zk_login::{OIDCProvider, ZkLoginInputsReader};
use crate::bn254::zk_login_api::Bn254Fr;
use crate::zk_login_utils::Bn254FrElement;
use fastcrypto::error::FastCryptoError;
use fastcrypto::hash::{Blake2b256, HashFunction};
use fastcrypto::rsa::Base64UrlUnpadded;
use fastcrypto::rsa::Encoding;
use num_bigint::BigUint;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;

use super::zk_login::hash_ascii_str_to_field;

const ZK_LOGIN_AUTHENTICATOR_FLAG: u8 = 0x05;
const MAX_KEY_CLAIM_NAME_LENGTH: u8 = 32;
const MAX_KEY_CLAIM_VALUE_LENGTH: u8 = 115;
const MAX_AUD_VALUE_LENGTH: u8 = 145;

/// Calculate the Sui address based on address seed and address params.
pub fn get_zk_login_address(
    address_seed: &Bn254FrElement,
    iss: &str,
) -> Result<[u8; 32], FastCryptoError> {
    let mut hasher = Blake2b256::default();
    hasher.update([ZK_LOGIN_AUTHENTICATOR_FLAG]);
    let bytes = iss.as_bytes();
    hasher.update([bytes.len() as u8]);
    hasher.update(bytes);
    hasher.update(address_seed.padded());
    Ok(hasher.finalize().digest)
}

/// Calculate the Sui address based on address seed and address params.
pub fn gen_address_seed(
    salt: &str,
    name: &str,  // i.e. "sub"
    value: &str, // i.e. the sub value
    aud: &str,   // i.e. the client ID
) -> Result<String, FastCryptoError> {
    let salt_hash = poseidon_zk_login(&[(&Bn254FrElement::from_str(salt)?).into()])?;
    gen_address_seed_with_salt_hash(&salt_hash.to_string(), name, value, aud)
}

/// Same as [`gen_address_seed`] but takes the poseidon hash of the salt as input instead of the salt.
pub(crate) fn gen_address_seed_with_salt_hash(
    salt_hash: &str,
    name: &str,  // i.e. "sub"
    value: &str, // i.e. the sub value
    aud: &str,   // i.e. the client ID
) -> Result<String, FastCryptoError> {
    Ok(poseidon_zk_login(&[
        hash_ascii_str_to_field(name, MAX_KEY_CLAIM_NAME_LENGTH)?,
        hash_ascii_str_to_field(value, MAX_KEY_CLAIM_VALUE_LENGTH)?,
        hash_ascii_str_to_field(aud, MAX_AUD_VALUE_LENGTH)?,
        (&Bn254FrElement::from_str(salt_hash)?).into(),
    ])?
    .to_string())
}

/// Return the OIDC URL for the given parameters. Crucially the nonce is computed.
pub fn get_oidc_url(
    provider: OIDCProvider,
    eph_pk_bytes: &[u8],
    max_epoch: u64,
    client_id: &str,
    redirect_url: &str,
    jwt_randomness: &str,
) -> Result<String, FastCryptoError> {
    let nonce = get_nonce(eph_pk_bytes, max_epoch, jwt_randomness)?;
    Ok(match provider {
            OIDCProvider::Google => format!("https://accounts.google.com/o/oauth2/v2/auth?client_id={}&response_type=id_token&redirect_uri={}&scope=openid&nonce={}", client_id, redirect_url, nonce),
            OIDCProvider::Twitch => format!("https://id.twitch.tv/oauth2/authorize?client_id={}&force_verify=true&lang=en&login_type=login&redirect_uri={}&response_type=id_token&scope=openid&nonce={}", client_id, redirect_url, nonce),
            OIDCProvider::Facebook => format!("https://www.facebook.com/v17.0/dialog/oauth?client_id={}&redirect_uri={}&scope=openid&nonce={}&response_type=id_token", client_id, redirect_url, nonce),
            OIDCProvider::Kakao => format!("https://kauth.kakao.com/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&nonce={}", client_id, redirect_url, nonce),
            OIDCProvider::Apple => format!("https://appleid.apple.com/auth/authorize?client_id={}&redirect_uri={}&scope=email&response_mode=form_post&response_type=code%20id_token&nonce={}", client_id, redirect_url, nonce),
            OIDCProvider::Slack => format!("https://slack.com/openid/connect/authorize?response_type=code&client_id={}&redirect_uri={}&nonce={}&scope=openid", client_id, redirect_url, nonce),
            OIDCProvider::Microsoft => format!("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id={}&scope=openid&response_type=id_token&redirect_uri={}&nonce={}", client_id, redirect_url, nonce),
            OIDCProvider::KarrierOne => format!("https://accounts.karrier.one/Account/PhoneLogin?ReturnUrl=/connect/authorize?nonce={}&redirect_uri={}&response_type=id_token&scope=openid&client_id={}", nonce, redirect_url, client_id),
            OIDCProvider::Credenza3 => format!("https://accounts.credenza3.com/oauth2/authorize?client_id={}&response_type=token&scope=openid+profile+email+phone&redirect_uri={}&nonce={}&state=state", client_id, redirect_url, nonce),
            OIDCProvider::Onefc => format!("https://login.onepassport.onefc.com/de3ee5c1-5644-4113-922d-e8336569a462/b2c_1a_prod_signupsignin_onesuizklogin/oauth2/v2.0/authorize?client_id={}&scope=openid&response_type=id_token&redirect_uri={}&nonce={}", client_id, redirect_url, nonce),
            OIDCProvider::AwsTenant((region, tenant_id)) => format!("https://{}.auth.{}.amazoncognito.com/login?response_type=token&client_id={}&redirect_uri={}&nonce={}", tenant_id, region, client_id, redirect_url, nonce),
            // this URL is only useful if CLI testing from Sui is needed, can ignore if a frontend test plan is in place
            _ => return Err(FastCryptoError::InvalidInput)
    })
}

/// Return the token exchange URL for the given auth code.
pub fn get_token_exchange_url(
    provider: OIDCProvider,
    client_id: &str,
    redirect_url: &str, // not required for Slack, pass in empty string.
    auth_code: &str,
    client_secret: &str, // not required for Kakao, pass in empty string.
) -> Result<String, FastCryptoError> {
    match provider {
        OIDCProvider::Kakao => Ok(format!("https://kauth.kakao.com/oauth/token?grant_type=authorization_code&client_id={}&redirect_uri={}&code={}", client_id, redirect_url, auth_code)),
        OIDCProvider::Slack => Ok(format!("https://slack.com/api/openid.connect.token?code={}&client_id={}&client_secret={}", auth_code, client_id, client_secret)),
        _ => Err(FastCryptoError::InvalidInput)
    }
}

/// Calculate the nonce for the given parameters. Nonce is defined as the Base64Url encoded of the poseidon hash of 4 inputs:
/// first half of eph_pk_bytes in BigInt, second half of eph_pk_bytes in BigInt, max_epoch and jwt_randomness.
pub fn get_nonce(
    eph_pk_bytes: &[u8],
    max_epoch: u64,
    jwt_randomness: &str,
) -> Result<String, FastCryptoError> {
    let (first, second) = split_to_two_frs(eph_pk_bytes)?;

    let max_epoch = Bn254Fr::from_str(&max_epoch.to_string())
        .expect("max_epoch.to_string is always non empty string without trailing zeros");
    let jwt_randomness =
        Bn254Fr::from_str(jwt_randomness).map_err(|_| FastCryptoError::InvalidInput)?;

    let hash = poseidon_zk_login(&[first, second, max_epoch, jwt_randomness])
        .expect("inputs is not too long");
    let data = BigUint::from(hash).to_bytes_be();
    let truncated = &data[data.len() - 20..];
    let mut buf = vec![0; Base64UrlUnpadded::encoded_len(truncated)];
    Ok(Base64UrlUnpadded::encode(truncated, &mut buf)
        .unwrap()
        .to_string())
}

/// A response struct for the salt server.
#[derive(Deserialize, Debug)]
pub struct GetSaltResponse {
    /// The salt in BigInt string.
    salt: String,
}

/// Call the salt server for the given jwt_token and return the salt.
pub async fn get_salt(jwt_token: &str, salt_url: &str) -> Result<String, FastCryptoError> {
    let client = Client::new();
    let body = json!({ "token": jwt_token });
    let response = client
        .post(salt_url)
        .json(&body)
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|_| FastCryptoError::InvalidInput)?;
    let full_bytes = response
        .bytes()
        .await
        .map_err(|_| FastCryptoError::InvalidInput)?;
    let res: GetSaltResponse =
        serde_json::from_slice(&full_bytes).map_err(|_| FastCryptoError::InvalidInput)?;
    Ok(res.salt)
}

/// Call the prover backend to get the zkLogin inputs based on jwt_token, max_epoch, jwt_randomness, eph_pubkey and salt.
pub async fn get_proof(
    jwt_token: &str,
    max_epoch: u64,
    jwt_randomness: &str,
    eph_pubkey: &str,
    salt: &str,
    prover_url: &str,
) -> Result<ZkLoginInputsReader, FastCryptoError> {
    let body = json!({
    "jwt": jwt_token,
    "extendedEphemeralPublicKey": eph_pubkey,
    "maxEpoch": max_epoch,
    "jwtRandomness": jwt_randomness,
    "salt": salt,
    "keyClaimName": "sub",
    });
    let client = Client::new();
    let response = client
        .post(prover_url.to_string())
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|_| FastCryptoError::InvalidInput)?;
    let full_bytes = response
        .bytes()
        .await
        .map_err(|_| FastCryptoError::InvalidInput)?;

    #[cfg(feature = "e2e")]
    println!("get_proof response: {:?}", full_bytes);

    let get_proof_response: ZkLoginInputsReader =
        serde_json::from_slice(&full_bytes).map_err(|_| FastCryptoError::InvalidInput)?;
    Ok(get_proof_response)
}

/// Given a 33-byte public key bytes (flag || pk_bytes), returns the two Bn254Fr split at the 128 bit index.
pub fn split_to_two_frs(eph_pk_bytes: &[u8]) -> Result<(Bn254Fr, Bn254Fr), FastCryptoError> {
    // Split the bytes deterministically such that the first element contains the first 128
    // bits of the hash, and the second element contains the latter ones.
    let (first_half, second_half) = eph_pk_bytes.split_at(eph_pk_bytes.len() - 16);
    let first_bigint = BigUint::from_bytes_be(first_half);
    // TODO: this is not safe if the buffer is large. Can we use a fixed size array for eph_pk_bytes?
    let second_bigint = BigUint::from_bytes_be(second_half);

    let eph_public_key_0 = Bn254Fr::from(first_bigint);
    let eph_public_key_1 = Bn254Fr::from(second_bigint);
    Ok((eph_public_key_0, eph_public_key_1))
}

/// Call test issuer for a JWT token based on the request parameters.
pub async fn get_test_issuer_jwt_token(
    client: &reqwest::Client,
    nonce: &str,
    iss: &str,
    sub: &str,
) -> Result<TestIssuerJWTResponse, FastCryptoError> {
    let response = client
        .post(format!(
            "https://jwt-tester.mystenlabs.com/jwt?nonce={}&iss={}&sub={}",
            nonce, iss, sub
        ))
        .header("Content-Type", "application/json")
        .header("Content-Length", "0")
        .send()
        .await
        .map_err(|_| FastCryptoError::InvalidInput)?;
    let full_bytes = response
        .bytes()
        .await
        .map_err(|_| FastCryptoError::InvalidInput)?;

    println!("get_jwt_response response: {:?}", full_bytes);

    let get_jwt_response: TestIssuerJWTResponse =
        serde_json::from_slice(&full_bytes).map_err(|_| FastCryptoError::InvalidInput)?;
    Ok(get_jwt_response)
}

/// The response struct for the test issuer JWT token.
#[derive(Debug, Serialize, Deserialize)]
pub struct TestIssuerJWTResponse {
    /// JWT token string.
    pub jwt: String,
}
