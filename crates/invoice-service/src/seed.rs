use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// Demo data. Three businesses, each with a job to do in the demo:
///
///   atlas    - the happy path. Its webhook endpoint accepts and verifies.
///   nova  - exists to prove tenant isolation: atlas's key must 404 here.
///   vertex - its endpoint always returns 500, so retries/backoff are visible.
///
pub struct Seed {
    pub business_id: &'static str,
    pub name: &'static str,
    pub api_key_id: &'static str,
    pub api_key: &'static str,
    pub endpoint_id: &'static str,
    pub webhook_path: &'static str,
    pub webhook_secret: &'static str,
}

pub const SEEDS: &[Seed] = &[
    Seed {
        business_id: "biz_atlas",
        name: "Atlas",
        api_key_id: "ak_seed_atlas",
        api_key: "dodo_sk_test_atlas_a1b2c3d4e5f6a7b8",
        endpoint_id: "whe_seed_atlas",
        webhook_path: "/hooks",
        webhook_secret: "whsec_test_atlas_9f3a7b2c1d4e5f60",
    },
    Seed {
        business_id: "biz_nova",
        name: "Nova",
        api_key_id: "ak_seed_nova",
        api_key: "dodo_sk_test_nova_c1d2e3f4a5b6c7d8",
        endpoint_id: "whe_seed_nova",
        webhook_path: "/hooks/slow",
        webhook_secret: "whsec_test_nova_2b4d6f8a0c1e3d50",
    },
    Seed {
        business_id: "biz_vertex",
        name: "Vertex",
        api_key_id: "ak_seed_vertex",
        api_key: "dodo_sk_test_vertex_e1f2a3b4c5d6e7f8",
        endpoint_id: "whe_seed_vertex",
        webhook_path: "/hooks/always-fail",
        webhook_secret: "whsec_test_vertex_7c5a3e1b9d0f2a40",
    },
];

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Idempotent, no duplicate seeding
pub async fn run(db: &PgPool, receiver_base_url: &str) -> anyhow::Result<()> {
    for s in SEEDS {
        sqlx::query(
            "INSERT INTO businesses (id, name) VALUES ($1, $2)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(s.business_id)
        .bind(s.name)
        .execute(db)
        .await?;

        sqlx::query(
            "INSERT INTO api_keys (id, business_id, key_hash, key_last4)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(s.api_key_id)
        .bind(s.business_id)
        .bind(sha256_hex(s.api_key)) // never the plaintext
        .bind(&s.api_key[s.api_key.len() - 4..])
        .execute(db)
        .await?;

        sqlx::query(
            "INSERT INTO webhook_endpoints (id, business_id, url, secret)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(s.endpoint_id)
        .bind(s.business_id)
        .bind(format!("{receiver_base_url}{}", s.webhook_path))
        .bind(s.webhook_secret)
        .execute(db)
        .await?;
    }

    tracing::info!("seeded {} demo businesses:", SEEDS.len());
    for s in SEEDS {
        tracing::info!("  {:<20} {}", s.name, s.api_key);
    }

    Ok(())
}
