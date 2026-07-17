"""Tests for the model pricing normalization and matching pipeline."""
import pytest
import cc_pricing


# ═══════════════════════════════════════════════════════════════════
#  helpers
# ═══════════════════════════════════════════════════════════════════

def _set_prices(monkeypatch, prices):
    """Replace _load_prices with *prices* and clear the index cache.

    Both _load_prices and _INDEX_CACHE are managed via monkeypatch for
    full automatic teardown — no manual restoration needed.
    """
    monkeypatch.setattr(cc_pricing, "_load_prices", lambda: dict(prices))
    monkeypatch.setattr(cc_pricing, "_INDEX_CACHE", None)


# ═══════════════════════════════════════════════════════════════════
#  test price catalogs
# ═══════════════════════════════════════════════════════════════════

PRICE_SONNET  = (3.0, 3.75, 0.3, 15.0)   # tuples — verify immutability
PRICE_DSV4    = (0.435, 0.0, 0.003625, 0.87)
PRICE_GPT56   = (5.0, 6.25, 0.5, 30.0)
PRICE_CODEX   = (1.75, 0.0, 0.175, 14.0)
PRICE_EXPENSIVE_SONNET = (4.0, 5.0, 0.4, 20.0)

SINGLE_DATE = {
    "claude-sonnet-4-5-20250929": PRICE_SONNET,
    "gpt-5.6-sol":                PRICE_GPT56,
    "gpt-5.2-codex":              PRICE_CODEX,
    "gpt-5.2-codex-low":          PRICE_CODEX,
    "deepseek-v4-pro":            PRICE_DSV4,
}

DUAL_DATE_SAME_PRICE = {
    "claude-sonnet-4-5-20250929": PRICE_SONNET,
    "claude-sonnet-4-5-20251015": PRICE_SONNET,
}

DUAL_DATE_DIFF_PRICE = {
    "claude-sonnet-4-5-20250929": PRICE_SONNET,
    "claude-sonnet-4-5-20251015": PRICE_EXPENSIVE_SONNET,
}

NO_BASE_ONLY_VARIANT = {
    "gpt-5.2-codex-low": PRICE_CODEX,
}


# ═══════════════════════════════════════════════════════════════════
#  exact match
# ═══════════════════════════════════════════════════════════════════

class TestExactMatch:
    PRICES = {
        "claude-sonnet-4-5-20250929": PRICE_SONNET,
        "gpt-5.6-sol":                PRICE_GPT56,
        "deepseek-v4-pro":            PRICE_DSV4,
    }

    def test_dated_model(self, monkeypatch):
        _set_prices(monkeypatch, self.PRICES)
        assert cc_pricing.prices_for("claude-sonnet-4-5-20250929") == PRICE_SONNET

    def test_simple_key(self, monkeypatch):
        _set_prices(monkeypatch, self.PRICES)
        assert cc_pricing.prices_for("gpt-5.6-sol") == PRICE_GPT56

    def test_deepseek_v4_pro(self, monkeypatch):
        _set_prices(monkeypatch, self.PRICES)
        assert cc_pricing.prices_for("deepseek-v4-pro") == PRICE_DSV4


# ═══════════════════════════════════════════════════════════════════
#  input boundaries
# ═══════════════════════════════════════════════════════════════════

class TestBoundaries:
    @pytest.mark.parametrize("value", ["", "   ", None, 42])
    def test_invalid_input_returns_none(self, monkeypatch, value):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for(value) is None

    def test_leading_trailing_spaces(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("  claude-sonnet-4-5  ") == PRICE_SONNET

    def test_empty_provider_segment_returns_none(self, monkeypatch):
        """Sole provider prefix with no model ID produces no match."""
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("anthropic/") is None


# ═══════════════════════════════════════════════════════════════════
#  normalization rules (single transformations)
# ═══════════════════════════════════════════════════════════════════

class TestNormalization:
    def test_undated_alias_matches_dated_key(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("claude-sonnet-4-5") == PRICE_SONNET

    def test_provider_prefix(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("anthropic/claude-sonnet-4-5") == PRICE_SONNET

    def test_dot_separators(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("anthropic/claude-sonnet-4.5") == PRICE_SONNET

    def test_colon_suffix(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("anthropic/claude-sonnet-4-5:beta") == PRICE_SONNET

    def test_at_variant(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("gpt-5.2-codex@low") == PRICE_CODEX

    def test_1m_suffix(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("claude-sonnet-4-5[1m]") == PRICE_SONNET

    def test_case_insensitive(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("Claude-Sonnet-4-5") == PRICE_SONNET


# ═══════════════════════════════════════════════════════════════════
#  combined normalization
# ═══════════════════════════════════════════════════════════════════

class TestCombinedNormalization:
    def test_all_rules_together(self, monkeypatch):
        """Provider prefix + mixed case + dots + colon + [1m] at once."""
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for(
            "Anthropic/Claude-Sonnet-4.5:beta[1m]"
        ) == PRICE_SONNET

    def test_underscore_and_dot_combo(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for(
            "CLAUDE_SONNET_4.5"
        ) == PRICE_SONNET

    def test_multiple_slashes(self, monkeypatch):
        """Only the path after the *last* / is kept (when provider stripped)."""
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for(
            "openrouter/anthropic/claude-sonnet-4-5"
        ) == PRICE_SONNET


# ═══════════════════════════════════════════════════════════════════
#  _normalize_model_id — direct unit tests (remove_provider flag)
# ═══════════════════════════════════════════════════════════════════

class TestNormalizeModelId:
    def test_remove_provider_strips_last_segment(self):
        assert cc_pricing._normalize_model_id(
            "anthropic/claude-sonnet-4-5", remove_provider=True,
        ) == "claude-sonnet-4-5"

    def test_keep_provider_preserves_full_string(self):
        """remove_provider=False keeps slash and provider prefix intact."""
        assert cc_pricing._normalize_model_id(
            "anthropic/claude-sonnet-4-5", remove_provider=False,
        ) == "anthropic/claude-sonnet-4-5"

    def test_double_slash_remove_provider(self):
        assert cc_pricing._normalize_model_id(
            "openrouter/anthropic/claude-sonnet-4-5", remove_provider=True,
        ) == "claude-sonnet-4-5"

    def test_non_date_numbers_preserved(self):
        """Version components like 3-5 in gpt-3-5-turbo are not dates."""
        assert cc_pricing._normalize_model_id(
            "gpt-3-5-turbo", remove_provider=False,
        ) == "gpt-3-5-turbo"

    @pytest.mark.parametrize("value, expected", [
        ("anthropic/", ""),
        ("/claude-sonnet-4-5", "claude-sonnet-4-5"),
        ("openrouter//claude-sonnet-4-5", "claude-sonnet-4-5"),
    ])
    def test_empty_provider_segments(self, value, expected):
        """Empty segments after / splits are handled gracefully."""
        assert cc_pricing._normalize_model_id(
            value, remove_provider=True,
        ) == expected


# ═══════════════════════════════════════════════════════════════════
#  variant vs base matching
# ═══════════════════════════════════════════════════════════════════

class TestVariantMatching:
    def test_base_matches_base_not_variant(self, monkeypatch):
        """gpt-5.2-codex should match its own key, not -low variant."""
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("gpt-5.2-codex") == PRICE_CODEX

    def test_variant_exists_base_missing(self, monkeypatch):
        """When only gpt-5.2-codex-low exists, gpt-5.2-codex gets no match."""
        _set_prices(monkeypatch, NO_BASE_ONLY_VARIANT)
        assert cc_pricing.prices_for("gpt-5.2-codex") is None


# ═══════════════════════════════════════════════════════════════════
#  not found
# ═══════════════════════════════════════════════════════════════════

class TestNotFound:
    def test_unknown_model_returns_none(self, monkeypatch):
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for("my-unknown-model-v1") is None


# ═══════════════════════════════════════════════════════════════════
#  ambiguity safety
# ═══════════════════════════════════════════════════════════════════

class TestAmbiguity:
    def test_same_price_two_dates_returns_none(self, monkeypatch):
        _set_prices(monkeypatch, DUAL_DATE_SAME_PRICE)
        assert cc_pricing.prices_for("claude-sonnet-4-5") is None

    def test_different_price_two_dates_returns_none(self, monkeypatch):
        """Ambiguity is about count, not price equality."""
        _set_prices(monkeypatch, DUAL_DATE_DIFF_PRICE)
        assert cc_pricing.prices_for("claude-sonnet-4-5") is None


# ═══════════════════════════════════════════════════════════════════
#  date recognition edge cases
# ═══════════════════════════════════════════════════════════════════

class TestDateRecognition:
    def test_nondate_numbers_in_name_not_stripped(self, monkeypatch):
        """Ordinary numeric version components (e.g. 3-5) are not dates."""
        _set_prices(monkeypatch, {"gpt-3-5-turbo": PRICE_CODEX})
        assert cc_pricing.prices_for("gpt-3-5-turbo") == PRICE_CODEX

    def test_full_date_still_matches_exact(self, monkeypatch):
        """Dated input matches exact dated key directly."""
        _set_prices(monkeypatch, SINGLE_DATE)
        assert cc_pricing.prices_for(
            "claude-sonnet-4-5-20250929"
        ) == PRICE_SONNET

    def test_embedded_date_not_stripped(self, monkeypatch):
        """A date that is *not* at the end of the model ID is left alone.

        The regex only strips a trailing date suffix. ``20250929`` in
        ``claude-sonnet-4-5-20250929-abc`` is not trailing, so it is
        NOT removed.  The input therefore differs from the unadorned
        family key and should not match.
        """
        _set_prices(monkeypatch, {
            "claude-sonnet-4-5-abc": PRICE_SONNET,
        })
        assert cc_pricing.prices_for(
            "claude-sonnet-4-5-20250929-abc"
        ) is None


# ═══════════════════════════════════════════════════════════════════
#  cache behavior
# ═══════════════════════════════════════════════════════════════════

class TestCache:
    def test_index_is_reused_on_second_call(self, monkeypatch):
        """Second call to prices_for hits the cached index — no reload."""
        call_count = 0

        def counting():
            nonlocal call_count
            call_count += 1
            return dict(SINGLE_DATE)

        monkeypatch.setattr(cc_pricing, "_load_prices", counting)
        monkeypatch.setattr(cc_pricing, "_INDEX_CACHE", None)

        cc_pricing.prices_for("claude-sonnet-4-5-20250929")
        cc_pricing.prices_for("gpt-5.6-sol")
        assert call_count == 1  # cache hit — counting() was only called once

    def test_cache_cleared_and_rebuilt(self, monkeypatch):
        """Invalidating _INDEX_CACHE triggers a fresh reload."""
        call_count = 0

        def counting():
            nonlocal call_count
            call_count += 1
            return dict(SINGLE_DATE)

        monkeypatch.setattr(cc_pricing, "_load_prices", counting)
        monkeypatch.setattr(cc_pricing, "_INDEX_CACHE", None)

        cc_pricing.prices_for("claude-sonnet-4-5")
        assert call_count == 1

        # Simulate cache invalidation — clearing forces a rebuild.
        monkeypatch.setattr(cc_pricing, "_INDEX_CACHE", None)
        cc_pricing.prices_for("claude-sonnet-4-5")
        assert call_count == 2


# ═══════════════════════════════════════════════════════════════════
#  return value safety
# ═══════════════════════════════════════════════════════════════════

class TestReturnValue:
    def test_normalize_price_value_converts_list_to_tuple(self):
        """_normalize_price_value turns a 4-element list into a tuple."""
        result = cc_pricing._normalize_price_value([1.0, 2.0, 3.0, 4.0])
        assert result == (1.0, 2.0, 3.0, 4.0)
        assert isinstance(result, tuple)

    def test_normalize_price_value_converts_dict_to_tuple(self):
        """_normalize_price_value reads input/cache_write/cache_read/output."""
        result = cc_pricing._normalize_price_value({
            "input": 1.0, "cache_write": 2.0,
            "cache_read": 3.0, "output": 4.0,
        })
        assert result == (1.0, 2.0, 3.0, 4.0)
        assert isinstance(result, tuple)

    def test_tuple_is_immutable(self, monkeypatch):
        """Callers cannot accidentally mutate the returned tuple.

        The assertion on the value MUST come first — if prices_for
        unexpectedly returns None, ``None[0]`` also raises TypeError
        and the test would pass for the wrong reason.
        """
        _set_prices(monkeypatch, SINGLE_DATE)
        result = cc_pricing.prices_for("claude-sonnet-4-5-20250929")
        assert result == PRICE_SONNET
        assert isinstance(result, tuple)
        with pytest.raises(TypeError):
            result[0] = 999.0


# ═══════════════════════════════════════════════════════════════════
#  catalog swap: verify stale cache is not reused
# ═══════════════════════════════════════════════════════════════════

class TestCatalogSwap:
    def test_replacing_catalog_does_not_reuse_stale_cache(self, monkeypatch):
        """Keys from a previous catalog must not survive a catalog swap."""
        _set_prices(monkeypatch, {"only-in-test": PRICE_SONNET})
        assert cc_pricing.prices_for("only-in-test") == PRICE_SONNET

        # Replace the entire catalog — the old key should be gone.
        _set_prices(monkeypatch, {"other-model": PRICE_DSV4})
        assert cc_pricing.prices_for("only-in-test") is None


# ═══════════════════════════════════════════════════════════════════
#  extract_usage — token field mapping (Anthropic / OpenAI formats)
# ═══════════════════════════════════════════════════════════════════

class TestExtractUsageAnthropic:
    """Anthropic-style usage: input_tokens, output_tokens,
       cache_creation_input_tokens, cache_read_input_tokens."""

    def test_standard_fields(self):
        result = cc_pricing.extract_usage({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_creation_input_tokens": 200,
            "cache_read_input_tokens": 300,
        })
        assert result == {"input": 1000, "output": 500,
                          "cache_write": 200, "cache_read": 300}

    def test_missing_cache_fields_default_to_zero(self):
        result = cc_pricing.extract_usage({
            "input_tokens": 100,
            "output_tokens": 50,
        })
        assert result["cache_write"] == 0
        assert result["cache_read"] == 0


class TestExtractUsageOpenAI:
    """OpenAI-style usage: prompt_tokens, completion_tokens,
       prompt_tokens_details.cached_tokens."""

    def test_prompt_and_completion_tokens(self):
        result = cc_pricing.extract_usage({
            "prompt_tokens": 2000,
            "completion_tokens": 800,
        })
        assert result["input"] == 2000
        assert result["output"] == 800

    def test_cached_tokens_subtracted_from_input(self):
        result = cc_pricing.extract_usage({
            "prompt_tokens": 2000,
            "completion_tokens": 800,
            "prompt_tokens_details": {"cached_tokens": 500},
        })
        assert result["input"] == 1500   # 2000 - 500
        assert result["cache_read"] == 500

    def test_cached_equals_input_subtracts_to_zero(self):
        result = cc_pricing.extract_usage({
            "prompt_tokens": 500,
            "completion_tokens": 10,
            "prompt_tokens_details": {"cached_tokens": 500},
        })
        assert result["input"] == 0
        assert result["cache_read"] == 500

    def test_cached_exceeds_input_is_clamped(self):
        """When cached > prompt_tokens, cache_read is clamped to input size.

        Only ``prompt_tokens`` actual tokens were sent; cached tokens
        cannot exceed that.  Clamping prevents double-counting.
        """
        result = cc_pricing.extract_usage({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "prompt_tokens_details": {"cached_tokens": 200},
        })
        assert result["input"] == 0       # 100 - min(200, 100)
        assert result["cache_read"] == 100  # clamped

    def test_zero_anthropic_cache_falls_back_to_openai_cached_tokens(self):
        """A zero Anthropic cache value allows OpenAI cached tokens as fallback."""
        result = cc_pricing.extract_usage({
            "input_tokens": 500,
            "output_tokens": 50,
            "cache_read_input_tokens": 0,           # zero → treated as absent
            "prompt_tokens_details": {"cached_tokens": 200},
        })
        assert result["cache_read"] == 200
        assert result["input"] == 300  # 500 - 200


class TestExtractUsageBoundaries:
    def test_non_dict_returns_zeros(self):
        for bad in [None, "abc", [], 42]:
            result = cc_pricing.extract_usage(bad)
            assert result == {"input": 0, "output": 0,
                              "cache_write": 0, "cache_read": 0}

    def test_none_values_fallback_to_zero(self):
        result = cc_pricing.extract_usage({
            "input_tokens": None,
            "output_tokens": None,
        })
        assert result["input"] == 0
        assert result["output"] == 0

    def test_cache_read_input_tokens_overrides_openai_style(self):
        """When both Anthropic and OpenAI fields are present, Anthropic wins
           because cache_read_input_tokens is checked first."""
        result = cc_pricing.extract_usage({
            "input_tokens": 2000,
            "output_tokens": 800,
            "cache_read_input_tokens": 300,
            "prompt_tokens": 9999,
            "prompt_tokens_details": {"cached_tokens": 9999},
        })
        assert result["cache_read"] == 300
        assert result["input"] == 2000  # Anthropic path — no subtraction

    def test_anthropic_input_tokens_priority_over_prompt_tokens(self):
        """input_tokens wins over prompt_tokens when both are present."""
        result = cc_pricing.extract_usage({
            "input_tokens": 100,
            "prompt_tokens": 999,
            "output_tokens": 50,
            "completion_tokens": 888,
        })
        assert result["input"] == 100
        assert result["output"] == 50

    def test_negative_token_values_flow_through(self):
        """Negative values are not sanitized — they pass through as-is."""
        result = cc_pricing.extract_usage({
            "input_tokens": -500,
            "output_tokens": -100,
        })
        assert result["input"] == -500
        assert result["output"] == -100


# ═══════════════════════════════════════════════════════════════════
#  cost_of — arithmetic sanity
# ═══════════════════════════════════════════════════════════════════

class TestCostOf:
    def test_known_model_computes_cost(self, monkeypatch):
        _set_prices(monkeypatch, {"model-a": [2.0, 0.5, 0.2, 8.0]})
        cost, known = cc_pricing.cost_of(
            {"input_tokens": 1_000_000, "output_tokens": 1_000_000,
             "cache_creation_input_tokens": 1_000_000,
             "cache_read_input_tokens": 1_000_000},
            "model-a",
        )
        # (1M*2.0 + 1M*0.5 + 1M*0.2 + 1M*8.0) / 1M = 10.7
        assert cost == pytest.approx(10.7)
        assert known is True

    def test_openai_cached_tokens_cost(self, monkeypatch):
        """End-to-end: OpenAI format with cached tokens routed through
           extract_usage → prices_for → cost_of."""
        _set_prices(monkeypatch, {"model-a": [2.0, 0.0, 0.5, 8.0]})
        cost, known = cc_pricing.cost_of({
            "prompt_tokens": 1_000_000,
            "completion_tokens": 0,
            "prompt_tokens_details": {"cached_tokens": 400_000},
        }, "model-a")
        # 600k normal input × $2.0  +  400k cached read × $0.5
        assert cost == pytest.approx(1.4)   # 0.6 * 2.0 + 0.4 * 0.5 = 1.4
        assert known is True

    def test_unknown_model_returns_zero_false(self, monkeypatch):
        _set_prices(monkeypatch, {"model-a": [2.0, 0.0, 0.0, 8.0]})
        cost, known = cc_pricing.cost_of(
            {"input_tokens": 1000}, "no-such-model",
        )
        assert cost == 0.0
        assert known is False

    def test_cached_exceeds_prompt_not_double_counted(self, monkeypatch):
        """Clamp prevents double-counting in the cost layer."""
        _set_prices(monkeypatch, {"model-a": [2.0, 0.0, 0.5, 8.0]})
        cost, known = cc_pricing.cost_of({
            "prompt_tokens": 100,
            "completion_tokens": 0,
            "prompt_tokens_details": {"cached_tokens": 200},
        }, "model-a")
        # 100 total input, all cached → 100 × $0.5 / 1M
        assert cost == pytest.approx(100 * 0.5 / 1_000_000)
        assert known is True

    def test_zero_usage_returns_zero_cost(self, monkeypatch):
        _set_prices(monkeypatch, {"model-a": [2.0, 0.5, 0.2, 8.0]})
        cost, known = cc_pricing.cost_of({}, "model-a")
        assert cost == 0.0
        assert known is True


# ═══════════════════════════════════════════════════════════════════
#  normalize_price_value — edge cases
# ═══════════════════════════════════════════════════════════════════

class TestNormalizePriceValue:
    def test_invalid_input_returns_none(self):
        for bad in [None, "abc", [], [1.0], [1.0, 2.0, 3.0], 42,
                    [1, 2, 3, 4, 5]]:
            assert cc_pricing._normalize_price_value(bad) is None

    def test_malformed_list_raises(self):
        """float('bad') raises ValueError — not silently swallowed."""
        with pytest.raises(ValueError):
            cc_pricing._normalize_price_value([1, 2, "bad", 4])

    def test_empty_dict_returns_zeros(self):
        assert cc_pricing._normalize_price_value({}) == (0.0, 0.0, 0.0, 0.0)

    def test_partial_dict_defaults_missing_to_zero(self):
        assert cc_pricing._normalize_price_value({
            "input": 1.0,
            "output": 4.0,
        }) == (1.0, 0.0, 0.0, 4.0)

    def test_none_values_in_dict_raise(self):
        """float(None) raises TypeError — dict None values are not defaulted."""
        with pytest.raises(TypeError):
            cc_pricing._normalize_price_value({
                "input": None, "cache_write": None,
                "cache_read": None, "output": None,
            })

    def test_tuple_input(self):
        assert cc_pricing._normalize_price_value((1.0, 2.0, 3.0, 4.0)) == (1.0, 2.0, 3.0, 4.0)

    def test_dict_extra_fields_ignored(self):
        assert cc_pricing._normalize_price_value({
            "input": 1.0, "cache_write": 2.0,
            "cache_read": 3.0, "output": 4.0,
            "extra": 999.0,
        }) == (1.0, 2.0, 3.0, 4.0)


# ═══════════════════════════════════════════════════════════════════
#  fmt_tokens / fmt_usd — display formatting
# ═══════════════════════════════════════════════════════════════════

class TestFormatting:
    # ── fmt_tokens ──────────────────────────────────────────────

    def test_fmt_tokens_zero(self):
        assert cc_pricing.fmt_tokens(0) == "0"

    def test_fmt_tokens_none(self):
        assert cc_pricing.fmt_tokens(None) == "0"

    def test_fmt_tokens_small(self):
        assert cc_pricing.fmt_tokens(42) == "42"

    @pytest.mark.parametrize("value, expected", [
        (999,        "999"),
        (1000,       "1.0K"),
        (1_500,      "1.5K"),
        (999_999,    "1000.0K"),
        (1_000_000,  "1.0M"),
        (2_500_000,  "2.5M"),
    ])
    def test_fmt_tokens_thresholds(self, value, expected):
        assert cc_pricing.fmt_tokens(value) == expected

    # ── fmt_usd ─────────────────────────────────────────────────

    def test_fmt_usd_none(self):
        assert cc_pricing.fmt_usd(None) == "$0.0000"

    def test_fmt_usd_zero(self):
        assert cc_pricing.fmt_usd(0.0) == "$0.0000"

    def test_fmt_usd_rounds_at_precision_boundary(self):
        """0.9999 with .3f rounds to 1.000 — format follows the spec."""
        assert cc_pricing.fmt_usd(0.9999) == "$1.000"

    @pytest.mark.parametrize("value, expected", [
        (0.00001, "$0.0000"),
        (0.0015,  "$0.0015"),
        (1.0,     "$1.00"),
        (2.5,     "$2.50"),
    ])
    def test_fmt_usd_thresholds(self, value, expected):
        assert cc_pricing.fmt_usd(value) == expected
