import { describe, expect, it } from "vitest";
import {
  calculateCombinedUsage,
  filterDisconnectedCandidates,
  formatPercentage,
  metricUsedAmount,
  type AllowanceMetric,
  type LocalAccountCandidate,
  type ProviderSnapshot,
} from "./App";

function snapshot(accountId: string, metrics: AllowanceMetric[]): ProviderSnapshot {
  return {
    accountId,
    provider: "github",
    label: accountId,
    scope: "Personal account",
    freshness: "fresh",
    updatedAt: "2026-09-15T00:00:00.000Z",
    metrics,
  };
}

describe("calculateCombinedUsage", () => {
  it("uses explicit remaining instead of source-reported consumed", () => {
    const result = calculateCombinedUsage([
      snapshot("account-a", [
        { kind: "credits", label: "Credits", unit: "credits", consumed: 10, limit: 100, remaining: 25 },
      ]),
    ]);

    expect(result.percentage).toBe(75);
    expect(result.unitPercentages).toEqual({ credits: 75 });
  });

  describe("filterDisconnectedCandidates", () => {
    it("hides local suggestions that are already connected", () => {
      const candidate = (id: string): LocalAccountCandidate => ({
        id,
        provider: "anthropic",
        label: id,
        account: "Local account",
        source: "Claude Code CLI",
        canConnect: true,
        connected: false,
        message: "",
      });

      expect(filterDisconnectedCandidates(
        [candidate("connected"), candidate("available")],
        [snapshot("connected", [])],
      ).map((account) => account.id)).toEqual(["available"]);
    });
  });

  it("weights same-unit usage by aggregating raw used and limits across accounts", () => {
    const result = calculateCombinedUsage([
      snapshot("account-a", [
        { kind: "credits", label: "Credits", unit: "credits", consumed: 80, limit: 100 },
      ]),
      snapshot("account-b", [
        { kind: "credits", label: "Credits", unit: "credits", consumed: 50, limit: 300 },
      ]),
    ]);

    expect(result.percentage).toBe(32.5);
    expect(result.unitPercentages).toEqual({ credits: 32.5 });
    expect(result.accountCount).toBe(2);
  });

  it("clamps each used amount to its own allowance before aggregation", () => {
    const result = calculateCombinedUsage([
      snapshot("remaining-above-limit", [
        { kind: "credits", label: "Credits", unit: "AIC", consumed: 80, limit: 100, remaining: 125 },
      ]),
      snapshot("negative-remaining", [
        { kind: "credits", label: "Credits", unit: "AIC", consumed: 20, limit: 100, remaining: -10 },
      ]),
      snapshot("consumed-above-limit", [
        { kind: "credits", label: "Credits", unit: "AIC", consumed: 150, limit: 100 },
      ]),
    ]);

    expect(result.unitPercentages.aic).toBeCloseTo(200 / 3);
    expect(result.percentage).toBeCloseTo(200 / 3);
    expect(metricUsedAmount({
      kind: "credits",
      label: "Credits",
      unit: "AIC",
      consumed: 80,
      limit: 100,
      remaining: 125,
    })).toBe(0);
  });

  it("averages unlike-unit percentages without mixing their raw values", () => {
    const result = calculateCombinedUsage([
      snapshot("account-a", [
        { kind: "currency", label: "Spend", unit: "USD", consumed: 50, limit: 100 },
      ]),
      snapshot("account-b", [
        { kind: "tokens", label: "Tokens", unit: "tokens", consumed: 900, limit: 1000 },
      ]),
    ]);

    expect(result.unitPercentages).toEqual({ usd: 50, tokens: 90 });
    expect(result.percentage).toBe(70);
  });

  it("returns unavailable when no snapshot has a positive authoritative limit", () => {
    const result = calculateCombinedUsage([
      snapshot("account-a", [
        { kind: "tokens", label: "Tokens", unit: "tokens", consumed: 500 },
        { kind: "credits", label: "Credits", unit: "credits", consumed: 20, limit: 0 },
      ]),
    ]);

    expect(result).toEqual({
      percentage: undefined,
      accountCount: 0,
      unitPercentages: {},
    });
  });

  describe("formatPercentage", () => {
    it("keeps small nonzero percentages meaningful", () => {
      expect(formatPercentage(0.2507)).toBe("0.25%");
      expect(formatPercentage(0.001)).toBe("<0.01%");
    });

    it("does not display 100 percent before the allowance is exhausted", () => {
      expect(formatPercentage(99.75)).toBe("99.75%");
      expect(formatPercentage(99.999)).toBe("99.99%");
      expect(formatPercentage(100)).toBe("100%");
    });
  });
});
