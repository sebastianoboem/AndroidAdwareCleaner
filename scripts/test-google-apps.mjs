import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { isGoogleApp } from "../src/googleApps.mjs";

describe("isGoogleApp", () => {
    it("matches com.google.android.*", () => {
    assert.equal(isGoogleApp("com.google.android.gms"), true);
    assert.equal(isGoogleApp("com.google.android.apps.maps"), true);
    assert.equal(isGoogleApp("com.google.android.youtube"), true);
    assert.equal(isGoogleApp("com.google.android.gm"), true);
  });

  it("matches Chrome exactly", () => {
    assert.equal(isGoogleApp("com.android.chrome"), true);
  });

  it("rejects other com.google.* without .android", () => {
    assert.equal(isGoogleApp("com.google.ar.core"), false);
    assert.equal(isGoogleApp("com.google.protobuf"), false);
  });

  it("rejects unrelated packages", () => {
    assert.equal(isGoogleApp("com.samsung.android.app"), false);
    assert.equal(isGoogleApp("com.android.vending"), false);
    assert.equal(isGoogleApp("com.google"), false);
    assert.equal(isGoogleApp("com.google.android"), false);
  });

  it("rejects non-string package names", () => {
    assert.equal(isGoogleApp(null), false);
    assert.equal(isGoogleApp(undefined), false);
    assert.equal(isGoogleApp(42), false);
  });
});
