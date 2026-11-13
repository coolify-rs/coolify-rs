//! `App\Services\ImageScan\ImageStartPin`: the exact build of a "Pull latest" security update:
//! the digest that Coolify scanned, waiting for the next start of the resource that runs it (a
//! Docker image or Compose app, a standalone database, or a service). The start pulls that
//! digest and gives it the configured tag, so the containers keep the tag (`mysql:8`) but run
//! the scanned build, even when the publisher pushed another build since. The configuration
//! never changes, so later starts pull the tag again.
//!
//! The pins live in the cache of the scans ([`super::super::cache`]): in memory, gone after a
//! restart of coolify-rs, after which the start pulls the tag as usual.

use std::time::Duration;

use coolify_db::image_scans::ScanOwner;
use coolify_remote::text::php_trim;
use md5::Digest as _;
use serde_json::{Value, json};

use super::super::cache;
use super::super::commands::InvalidArgument;
use super::super::image_ref::ImageRef;
use crate::compose_parser::php::escapeshellarg;

/// `ImageStartPin::TTL_HOURS`: a queued deploy can wait; after this the start pulls the tag as
/// usual and the scan after it checks the result.
const TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// One waiting build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// `ref`: the image as the start names it (`mysql:8`, the scanned image ref).
    pub reference: String,
    /// `pull`: the scanned build (`docker.io/library/mysql@sha256:…`).
    pub pull: String,
}

/// The pins of an owner, keyed by the normalized image ref, in insertion order (upstream's
/// `array<string, array{ref: string, pull: string}>`).
pub type Pins = Vec<(String, Pin)>;

/// `ImageStartPin::put($owner, $ref, $digest)`: `reference` is the image as the start names it,
/// `digest` the scanned digest of the newer build (`sha256:…`).
pub fn put(owner: &ScanOwner, reference: &str, digest: &str) -> Result<(), InvalidArgument> {
    let image = ImageRef::parse(reference)?;
    if image.digest.is_some() || !ImageRef::is_valid_digest(digest) {
        return Err(InvalidArgument(
            "A pin needs an image tag and a digest.".to_owned(),
        ));
    }

    let mut pins = peek(owner);
    let name = normalize(reference);
    let pin = Pin {
        reference: reference.to_owned(),
        pull: format!("{}@{digest}", image.qualified_name()),
    };
    match pins.iter_mut().find(|(existing, _)| *existing == name) {
        Some(slot) => slot.1 = pin,
        None => pins.push((name, pin)),
    }
    cache::put(&key(owner), to_value(&pins), TTL);
    Ok(())
}

/// `ImageStartPin::take($owner, $server)`: the pins of the owner, which the start uses up. With
/// a server, each server gets the same pins once until they expire: an app that deploys to more
/// servers runs one deploy per server, and each pulls the scanned build.
pub fn take(owner: &ScanOwner, server_id: Option<i64>) -> Pins {
    let Some(server) = server_id else {
        // `Cache::pull()`: get, then forget.
        let pins = peek(owner);
        cache::forget(&key(owner));
        return pins;
    };

    let pins = peek(owner);
    if pins.is_empty() {
        return pins;
    }

    // A new pin of the owner changes the set, so every server takes it again.
    let taken = format!("{}:taken:{server}:{}", key(owner), fingerprint(&pins));
    if cache::add(&taken, Value::Bool(true), TTL) {
        pins
    } else {
        Vec::new()
    }
}

/// `ImageStartPin::forget($owner)`.
pub fn forget(owner: &ScanOwner) {
    cache::forget(&key(owner));
}

/// `ImageStartPin::for($pins, $images)`: only the pins of the given images (as a start names
/// them), for a start that runs some of them.
#[allow(clippy::ptr_arg)]
pub fn for_images(pins: &Pins, images: &[String]) -> Pins {
    let names: Vec<String> = images.iter().map(|image| normalize(image)).collect();
    pins.iter()
        .filter(|(name, _)| names.contains(name))
        .cloned()
        .collect()
}

/// `ImageStartPin::commands($pins, $prefix)`: pulls the exact build and gives it the configured
/// tag. When the build cannot be pulled (a registry error, or the publisher removed it), the
/// start pulls the tag as usual and the scan after it checks the result. `prefix` is the Docker
/// config prefix of the server.
#[allow(clippy::ptr_arg)]
pub fn commands(pins: &Pins, prefix: &str) -> Vec<String> {
    let mut commands = Vec::with_capacity(pins.len() * 2);
    for (_, pin) in pins {
        commands.push(format!(
            "echo {}",
            escapeshellarg(&format!("Pulling the scanned build of {}.", pin.reference))
        ));
        let pull = escapeshellarg(&pin.pull);
        let reference = escapeshellarg(&pin.reference);
        commands.push(format!(
            "{prefix}docker pull {pull} && {prefix}docker tag {pull} {reference} || {prefix}docker pull {reference}"
        ));
    }
    commands
}

/// `ImageStartPin::normalize($ref)`: `mysql:8`, `docker.io/library/mysql:8`, and
/// `index.docker.io/mysql:8` are the same image.
fn normalize(reference: &str) -> String {
    let reference = php_trim(reference);
    match ImageRef::parse(reference) {
        Ok(image) => format!(
            "{}:{}",
            image.qualified_name(),
            image.tag.as_deref().unwrap_or("latest")
        ),
        Err(_) => reference.to_owned(),
    }
}

/// `ImageStartPin::peek($owner)`.
fn peek(owner: &ScanOwner) -> Pins {
    cache::get(&key(owner))
        .as_ref()
        .map(from_value)
        .unwrap_or_default()
}

/// `ImageStartPin::key($owner)`.
fn key(owner: &ScanOwner) -> String {
    format!("image-scan:start-pin:{}:{}", owner.morph, owner.id)
}

/// The pins as the cache keeps them: a list, so the order stays.
fn to_value(pins: &[(String, Pin)]) -> Value {
    Value::Array(
        pins.iter()
            .map(|(name, pin)| json!({"name": name, "ref": pin.reference, "pull": pin.pull}))
            .collect(),
    )
}

fn from_value(value: &Value) -> Pins {
    let text = |entry: &Value, key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
    value
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some((
                        text(entry, "name")?,
                        Pin {
                            reference: text(entry, "ref")?,
                            pull: text(entry, "pull")?,
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Upstream's `md5(serialize($pins))`. It only names the set of pins in the cache key of
/// `take()` with a server, so any stable hash of the pins does: here the MD5 of their JSON.
fn fingerprint(pins: &[(String, Pin)]) -> String {
    format!(
        "{:x}",
        md5::Md5::digest(to_value(pins).to_string().as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::image_scan::resource_scans::tests::owner as scan_owner;

    const DIGEST: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    /// A database of its own, so tests that run at the same time do not share pins.
    fn database() -> ScanOwner {
        static NEXT: AtomicI64 = AtomicI64::new(910_000);
        let mut owner = scan_owner(r"App\Models\StandalonePostgresql", json!({}));
        owner.id = NEXT.fetch_add(1, Ordering::Relaxed);
        owner
    }

    fn names(pins: &Pins) -> Vec<&str> {
        pins.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// `it('keeps the scanned digest per image until a start takes it')`.
    #[test]
    fn keeps_the_scanned_digest_per_image_until_a_start_takes_it() {
        let database = database();
        put(&database, "postgres:16-alpine", DIGEST).unwrap();

        assert_eq!(
            take(&database, None),
            vec![(
                "docker.io/library/postgres:16-alpine".to_owned(),
                Pin {
                    reference: "postgres:16-alpine".to_owned(),
                    pull: format!("docker.io/library/postgres@{DIGEST}"),
                }
            )]
        );
        assert!(take(&database, None).is_empty());
    }

    /// `it('gives the pins once to each server that starts the owner')`.
    #[test]
    fn gives_the_pins_once_to_each_server_that_starts_the_owner() {
        let database = database();
        put(&database, "postgres:16-alpine", DIGEST).unwrap();

        assert_eq!(
            names(&take(&database, Some(1))),
            ["docker.io/library/postgres:16-alpine"]
        );
        assert!(take(&database, Some(1)).is_empty());
        assert_eq!(
            names(&take(&database, Some(2))),
            ["docker.io/library/postgres:16-alpine"]
        );

        put(&database, "postgres:17-alpine", DIGEST).unwrap();

        assert_eq!(
            names(&take(&database, Some(1))),
            [
                "docker.io/library/postgres:16-alpine",
                "docker.io/library/postgres:17-alpine"
            ]
        );
        forget(&database);
        assert!(take(&database, Some(3)).is_empty());
    }

    /// `it('refuses an image pinned by digest or a digest that is not one')`.
    #[test]
    fn refuses_an_image_pinned_by_digest_or_a_digest_that_is_not_one() {
        let database = database();
        assert_eq!(
            put(&database, &format!("postgres@{DIGEST}"), DIGEST),
            Err(InvalidArgument(
                "A pin needs an image tag and a digest.".to_owned()
            ))
        );
        assert_eq!(
            put(&database, "postgres:16", "sha256:abc; rm -rf /"),
            Err(InvalidArgument(
                "A pin needs an image tag and a digest.".to_owned()
            ))
        );
        assert_eq!(
            put(&database, "Postgres:16!", DIGEST),
            Err(InvalidArgument("Invalid image reference.".to_owned()))
        );
        assert!(take(&database, None).is_empty());
    }

    #[test]
    fn a_second_pin_of_the_same_image_replaces_the_first() {
        let database = database();
        let newer = format!("sha256:{}", "d".repeat(64));
        put(&database, "postgres:16", DIGEST).unwrap();
        put(&database, "redis:7", DIGEST).unwrap();
        // The same image, written with its registry.
        put(&database, "docker.io/library/postgres:16", &newer).unwrap();

        assert_eq!(
            take(&database, None),
            vec![
                (
                    "docker.io/library/postgres:16".to_owned(),
                    Pin {
                        reference: "docker.io/library/postgres:16".to_owned(),
                        pull: format!("docker.io/library/postgres@{newer}"),
                    }
                ),
                (
                    "docker.io/library/redis:7".to_owned(),
                    Pin {
                        reference: "redis:7".to_owned(),
                        pull: format!("docker.io/library/redis@{DIGEST}"),
                    }
                ),
            ]
        );
    }

    /// `it('pulls the digest and gives it the tag, with quoted arguments')` and the command of
    /// `it('pulls the tag when the scanned build cannot be pulled, …')`.
    #[test]
    fn pulls_the_digest_and_gives_it_the_tag_with_quoted_arguments() {
        let pins: Pins = vec![(
            "x".to_owned(),
            Pin {
                reference: "ghcr.io/acme/web:2.0".to_owned(),
                pull: format!("ghcr.io/acme/web@{DIGEST}"),
            },
        )];

        let with_prefix = commands(&pins, "DOCKER_CONFIG=/auth ");
        assert_eq!(
            with_prefix,
            [
                "echo 'Pulling the scanned build of ghcr.io/acme/web:2.0.'".to_owned(),
                format!(
                    "DOCKER_CONFIG=/auth docker pull 'ghcr.io/acme/web@{DIGEST}' && DOCKER_CONFIG=/auth docker tag 'ghcr.io/acme/web@{DIGEST}' 'ghcr.io/acme/web:2.0' || DOCKER_CONFIG=/auth docker pull 'ghcr.io/acme/web:2.0'"
                ),
            ]
        );
        assert_eq!(
            commands(&pins, "")[1],
            format!(
                "docker pull 'ghcr.io/acme/web@{DIGEST}' && docker tag 'ghcr.io/acme/web@{DIGEST}' 'ghcr.io/acme/web:2.0' || docker pull 'ghcr.io/acme/web:2.0'"
            )
        );
        assert!(commands(&Vec::new(), "").is_empty());
    }

    /// `it('keeps only the pins of the images a start runs')`.
    #[test]
    fn keeps_only_the_pins_of_the_images_a_start_runs() {
        let pin = |reference: &str, pull: &str| Pin {
            reference: reference.to_owned(),
            pull: pull.to_owned(),
        };
        let pins: Pins = vec![
            (
                "docker.io/library/nginx:1.27".to_owned(),
                pin("nginx:1.27", "p1"),
            ),
            ("docker.io/library/redis:7".to_owned(), pin("redis:7", "p2")),
        ];

        assert_eq!(
            for_images(&pins, &["docker.io/library/nginx:1.27".to_owned()]),
            vec![(
                "docker.io/library/nginx:1.27".to_owned(),
                pin("nginx:1.27", "p1")
            )]
        );
        // The images are compared as the start names them.
        assert_eq!(
            names(&for_images(
                &pins,
                &[
                    " redis:7 ".to_owned(),
                    "index.docker.io/nginx:1.27".to_owned()
                ]
            )),
            ["docker.io/library/nginx:1.27", "docker.io/library/redis:7"]
        );
        assert!(for_images(&pins, &["nginx:1.28".to_owned()]).is_empty());
    }

    #[test]
    fn names_images_without_their_registry_spelling() {
        assert_eq!(normalize("mysql:8"), "docker.io/library/mysql:8");
        assert_eq!(
            normalize("docker.io/library/mysql:8"),
            "docker.io/library/mysql:8"
        );
        assert_eq!(
            normalize("index.docker.io/mysql:8"),
            "docker.io/library/mysql:8"
        );
        assert_eq!(normalize("mysql"), "docker.io/library/mysql:latest");
        assert_eq!(normalize(" ${IMAGE} "), "${IMAGE}");
    }

    #[test]
    fn the_key_names_the_owner() {
        let mut database = database();
        database.id = 12;
        assert_eq!(
            key(&database),
            r"image-scan:start-pin:App\Models\StandalonePostgresql:12"
        );
    }
}
