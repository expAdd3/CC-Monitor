"""Tests for the model pricing normalization and matching pipeline."""
import pytest
import cc_pricing


# ── helpers ───────────────────────────────────────────────────────

def _use_prices(prices):
    """Replace _load_prices with *prices* and rebuild the index."""
    cc_pricing._INDEX_CACHE = None
    cc_pricing._load_prices = lambda: dict(prices)


# ── exact match ───────────────────────────────────────────────────

class TestExactMatch:
    PRICES = {
        "claude-sonnet-4-5-20250929": [3.0, 3.75, 0.3, 15.0],
        "gpt-5.6-sol":                [5.0, 6.25, 0.5, 30.0],
        "deepseek-v4-pro":            [0.435, 0.0, 0.003625, 0.87],
    }

    def test_dated_model(self):
        _use_prices(self.PRICES)
        assert cc_pricing.prices_for("claude-sonnet-4-5-20250929") == self.PRICES["claude-sonnet-4-5-20250929"]

    def test_simple_key(self):
        _use_prices(self.PRICES)
        assert cc_pricing.prices_for("gpt-5.6-sol") == self.PRICES["gpt-5.6-sol"]

    def test_deepseek_v4_pro(self):
        _use_prices(self.PRICES)
        assert cc_pricing.prices_for("deepseek-v4-pro") == self.PRICES["deepseek-v4-pro"]


# ── normalization variants ────────────────────────────────────────
# Uses a single-dated-entry price set so the undated alias resolves.

PRICE_SONNET = [3.0, 3.75, 0.3, 15.0]
PRICE_DSV4   = [0.435, 0.0, 0.003625, 0.87]

SINGLE_DATE_PRICES = {
    "claude-sonnet-4-5-20250929": PRICE_SONNET,
    "gpt-5.6-sol":                [5.0, 6.25, 0.5, 30.0],
    "gpt-5.2-codex":              [1.75, 0.0, 0.175, 14.0],
    "gpt-5.2-codex-low":          [1.75, 0.0, 0.175, 14.0],
    "deepseek-v4-pro":            PRICE_DSV4,
}

PRICE_CODEX = [1.75, 0.0, 0.175, 14.0]


class TestNormalization:
    def test_undated_alias_matches_dated_key(self):
        _use_prices(SINGLE_DATE_PRICES)
        assert cc_pricing.prices_for("claude-sonnet-4-5") == PRICE_SONNET

    def test_provider_prefix(self):
        _use_prices(SINGLE_DATE_PRICES)
        assert cc_pricing.prices_for("anthropic/claude-sonnet-4-5") == PRICE_SONNET

    def test_dot_separators(self):
        _use_prices(SINGLE_DATE_PRICES)
        assert cc_pricing.prices_for("anthropic/claude-sonnet-4.5") == PRICE_SONNET

    def test_colon_suffix(self):
        _use_prices(SINGLE_DATE_PRICES)
        assert cc_pricing.prices_for("anthropic/claude-sonnet-4-5:beta") == PRICE_SONNET

    def test_at_variant(self):
        _use_prices(SINGLE_DATE_PRICES)
        assert cc_pricing.prices_for("gpt-5.2-codex@low") == PRICE_CODEX

    def test_1m_suffix(self):
        _use_prices(SINGLE_DATE_PRICES)
        assert cc_pricing.prices_for("claude-sonnet-4-5[1m]") == PRICE_SONNET

    def test_case_insensitive(self):
        _use_prices(SINGLE_DATE_PRICES)
        assert cc_pricing.prices_for("Claude-Sonnet-4-5") == PRICE_SONNET


# ── not found ─────────────────────────────────────────────────────

class TestNotFound:
    def test_unknown_model_returns_none(self):
        _use_prices(SINGLE_DATE_PRICES)
        assert cc_pricing.prices_for("my-unknown-model-v1") is None


# ── ambiguity safety ──────────────────────────────────────────────

DUAL_DATE_PRICES = {
    "claude-sonnet-4-5-20250929": PRICE_SONNET,
    "claude-sonnet-4-5-20251015": PRICE_SONNET,
}


class TestAmbiguity:
    def test_multiple_dated_versions_return_none(self):
        _use_prices(DUAL_DATE_PRICES)
        # Two dated entries for the same family — don't guess.
        assert cc_pricing.prices_for("claude-sonnet-4-5") is None
