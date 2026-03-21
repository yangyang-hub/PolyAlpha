use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy)]
pub struct CryptoDiscoveryAsset {
    pub name: &'static str,
    pub keywords: &'static [&'static str],
}

pub const CRYPTO_DISCOVERY_ASSETS: &[CryptoDiscoveryAsset] = &[
    CryptoDiscoveryAsset {
        name: "bitcoin",
        keywords: &["bitcoin", "btc"],
    },
    CryptoDiscoveryAsset {
        name: "ethereum",
        keywords: &["ethereum", "eth"],
    },
    CryptoDiscoveryAsset {
        name: "solana",
        keywords: &["solana", "sol"],
    },
    CryptoDiscoveryAsset {
        name: "bnb",
        keywords: &["bnb", "binance coin"],
    },
    CryptoDiscoveryAsset {
        name: "xrp",
        keywords: &["xrp", "ripple"],
    },
    CryptoDiscoveryAsset {
        name: "dogecoin",
        keywords: &["dogecoin", "doge"],
    },
    CryptoDiscoveryAsset {
        name: "cardano",
        keywords: &["cardano", "ada"],
    },
    CryptoDiscoveryAsset {
        name: "avalanche",
        keywords: &["avalanche", "avax"],
    },
    CryptoDiscoveryAsset {
        name: "polkadot",
        keywords: &["polkadot", "dot"],
    },
    CryptoDiscoveryAsset {
        name: "polygon",
        keywords: &["polygon", "matic", "pol"],
    },
];

pub fn crypto_search_terms() -> Vec<&'static str> {
    let mut terms: BTreeSet<&'static str> = BTreeSet::new();
    terms.insert("crypto price");
    terms.insert("crypto prices");

    for asset in CRYPTO_DISCOVERY_ASSETS {
        terms.insert(asset.name);
        terms.insert(match asset.name {
            "bitcoin" => "bitcoin price",
            "ethereum" => "ethereum price",
            "solana" => "solana price",
            "bnb" => "bnb price",
            "xrp" => "xrp price",
            "dogecoin" => "dogecoin price",
            "cardano" => "cardano price",
            "avalanche" => "avalanche price",
            "polkadot" => "polkadot price",
            "polygon" => "polygon price",
            _ => asset.name,
        });

        for keyword in asset.keywords {
            if keyword.len() <= 5 {
                terms.insert(keyword);
                terms.insert(match *keyword {
                    "btc" => "btc price",
                    "eth" => "eth price",
                    "sol" => "sol price",
                    "bnb" => "bnb price",
                    "xrp" => "xrp price",
                    "doge" => "doge price",
                    "ada" => "ada price",
                    "avax" => "avax price",
                    "dot" => "dot price",
                    "matic" => "matic price",
                    "pol" => "pol price",
                    _ => keyword,
                });
            }
        }
    }

    terms.into_iter().collect()
}

pub fn is_crypto_price_market_text(text: &str) -> bool {
    let lower = text.to_lowercase();

    if lower.contains("gas price")
        || lower.contains("gas fee")
        || lower.contains("volatility index")
        || lower.contains("dominance")
        || lower.contains("kimchi premium")
    {
        return false;
    }

    let has_asset = CRYPTO_DISCOVERY_ASSETS.iter().any(|asset| {
        asset.keywords.iter().any(|kw| contains_word(&lower, kw))
            || contains_word(&lower, asset.name)
    });
    let has_price_indicator = lower.contains('$')
        || lower.contains("price")
        || lower.contains("prices")
        || lower.contains("reach")
        || lower.contains("hit")
        || lower.contains("exceed")
        || lower.contains("dip")
        || lower.contains("above")
        || lower.contains("below");

    has_asset && has_price_indicator
}

fn contains_word(text: &str, word: &str) -> bool {
    if let Some(pos) = text.find(word) {
        let before_ok = pos == 0 || !text.as_bytes()[pos - 1].is_ascii_alphabetic();
        let after = pos + word.len();
        let after_ok = after >= text.len() || !text.as_bytes()[after].is_ascii_alphabetic();
        before_ok && after_ok
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_terms_cover_major_and_alt_assets() {
        let terms = crypto_search_terms();
        assert!(terms.contains(&"bitcoin price"));
        assert!(terms.contains(&"ethereum price"));
        assert!(terms.contains(&"solana price"));
        assert!(terms.contains(&"doge price"));
    }

    #[test]
    fn crypto_text_detection_rejects_non_price_markets() {
        assert!(is_crypto_price_market_text(
            "Will Bitcoin reach $150,000 by December?"
        ));
        assert!(is_crypto_price_market_text("Ethereum prices in 2026"));
        assert!(!is_crypto_price_market_text(
            "Will Ethereum gas price exceed 30 gwei?"
        ));
    }
}
