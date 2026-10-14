/**
 * Ask-gate layer 1 tests.
 *
 * Run with:  npm run test:gate
 * (Node's built-in test runner + type stripping — no test dependency added.)
 *
 * The load-bearing assertion is RECALL: every entry in REAL_REQUESTS must pass
 * the gate in BOTH modes. A false negative there means the app silently
 * swallowed a real question, which is far worse than paying for a "hmm".
 */

import { strict as assert } from "node:assert";
import { describe, it } from "node:test";

import { shouldAsk } from "./index.ts";
import {
  isBackchannel,
  isBelowLengthFloor,
  isNonSpeechAnnotation,
  normalizeTranscript,
  residualTokens,
  tokenize,
} from "./backchannel.ts";

/**
 * Fixture list 1 — backchannels and non-requests. Must be rejected in
 * hands-free (`continuous`) capture.
 */
const FILLERS: readonly string[] = [
  // non-lexical hesitation
  "hmm",
  "Hmm.",
  "hmmmm",
  "uh",
  "um...",
  "Uhh,",
  "er",
  "ahh",
  "oh",
  "mm",
  "ugh",
  // acknowledgements
  "ok",
  "Okay.",
  "OK!",
  "k",
  "yeah",
  "Yeah,",
  "yep",
  "yup",
  "yes",
  "no",
  "nope",
  "nah",
  "right",
  "Right.",
  "sure",
  "cool",
  "alright",
  "exactly",
  "correct",
  "true",
  "agreed",
  "perfect",
  "wow",
  "mhm",
  "mm-hmm",
  "uh-huh",
  "huh",
  // multi-word acknowledgements
  "got it",
  "Got it.",
  "gotcha",
  "I see.",
  "i know",
  "makes sense",
  "That makes sense.",
  "sounds good",
  "fair enough",
  "no worries",
  "never mind",
  "of course",
  "for sure",
  "all right",
  "you know",
  "i mean",
  "good point",
  "that's right",
  // discourse markers on their own
  "well",
  "so",
  "like",
  "actually",
  "basically",
  "anyway",
  "whatever",
  // social / STT hallucination on silence
  "thanks",
  "Thank you.",
  "Thank you so much.",
  "cheers",
  "sorry",
  "please",
  // Hinglish
  "haan",
  "haan haan",
  "accha",
  "acha",
  "theek hai",
  "thik hai",
  "ji haan",
  "bilkul",
  "arre",
  "matlab",
  "bas",
  "nahi",
  "sahi",
  "ha ha",
  // stacked filler
  "okay, sure",
  "yeah yeah",
  "hmm ok",
  "uh, yeah, right",
  "well, okay then",
  "no, thanks",
  "yes please",
  // non-speech annotations
  "[BLANK_AUDIO]",
  "...",
  ".",
  "♪",
  "♪ ♪",
  "(music)",
  "[inaudible]",
  "*laughs*",
  "   ",
  "",
  // lone non-command words picked up off a meeting
  "revenue",
  "Sarah",
  "interesting",
  "Tuesday",
];

/**
 * Fixture list 2 — real requests. MUST pass in BOTH modes. Zero tolerance.
 */
const REAL_REQUESTS: readonly string[] = [
  // plain questions
  "what is the capital of France",
  "who won the match yesterday",
  "why is the build failing?",
  "how do we handle the migration",
  "when is the renewal date",
  "which option is cheaper",
  "is the deal closed?",
  "are we over budget",
  "did they sign the contract",
  "can you summarize what we just discussed?",
  "could you pull up their last invoice",
  // filler-prefixed real questions — the highest-risk class
  "okay so what should I say next?",
  "hmm, what do you think about that pricing model?",
  "yeah but how do we handle the migration?",
  "right, so can you draft an email to the client?",
  "uh, what was the second point again",
  "so like what does that actually cost",
  "well, why did the numbers change",
  "no, I meant the other one",
  "ok cancel that and try again",
  "sure, but what about the tax implications",
  "got it, now write the follow up",
  "thanks, can you also list the risks",
  "actually, summarize the last five minutes",
  "haan, iska matlab kya hai",
  "theek hai, now explain the second option",
  // imperatives
  "summarize this",
  "translate that to Hindi",
  "explain the difference between a mutex and a semaphore",
  "give me three bullet points on this",
  "draft a follow up email",
  "book me a cab to the airport",
  "define idempotent",
  "compare option a and option b",
  "write a SQL query for monthly active users",
  "what's their revenue last quarter?",
  "how much did we spend",
  // one-word / very short but legitimate
  "continue",
  "summarize",
  "louder",
  "sources?",
  "why?",
  "what",
  "more details",
  "next steps",
  "tldr",
  "recap",
  // romanised Hindi — the filler list covers Hinglish, so the command side
  // has to as well or the length floor eats real one-word requests
  "batao",
  "samjhao",
  "likho",
  "ji bataiye",
  "na karo",
  "matlab kya hai",
  "email likho",
  "translate karo",
  "iska matlab batao",
  "wait",
  "hold on",
  // non-Latin script
  "हिंदी में समझाओ",
  "इसका मतलब क्या है",
];

/**
 * Fixture list 3 — short utterances that are legitimate commands once the user
 * has deliberately opened the mic. All must pass in `explicit` mode.
 */
const EXPLICIT_ONLY_COMMANDS: readonly string[] = [
  "yes",
  "no",
  "cancel it",
  "sure",
  "go on",
  "never mind",
  "okay",
  "yeah",
  "right",
  "stop",
  "bas",
  "matlab",
  "theek hai",
  "haan",
  "thanks",
];

/**
 * The subset of list 3 that is pure acknowledgement, i.e. the utterances whose
 * whole purpose is the mode split: meaningful on an explicit mic, noise in a
 * room. `"cancel it"` and `"stop"` are deliberately absent — they survive the
 * gate in both modes because they are unambiguous commands, and the recall
 * bias says keep them.
 */
const ACKNOWLEDGEMENTS_GATED_WHEN_HANDS_FREE: readonly string[] =
  EXPLICIT_ONLY_COMMANDS.filter(
    (transcript) => transcript !== "cancel it" && transcript !== "stop"
  );

describe("recall: real requests are never gated", () => {
  it("passes every real request in continuous mode", () => {
    const rejected = REAL_REQUESTS.filter(
      (transcript) => !shouldAsk({ transcript, mode: "continuous" }).ask
    );
    assert.deepEqual(
      rejected,
      [],
      `continuous mode rejected real requests: ${JSON.stringify(rejected)}`
    );
  });

  it("passes every real request in explicit mode", () => {
    const rejected = REAL_REQUESTS.filter(
      (transcript) => !shouldAsk({ transcript, mode: "explicit" }).ask
    );
    assert.deepEqual(
      rejected,
      [],
      `explicit mode rejected real requests: ${JSON.stringify(rejected)}`
    );
  });

  it("reports reason ok and an empty status when it passes", () => {
    for (const transcript of REAL_REQUESTS) {
      const decision = shouldAsk({ transcript, mode: "continuous" });
      assert.equal(decision.reason, "ok", transcript);
      assert.equal(decision.status, "", transcript);
    }
  });
});

describe("precision: backchannels are gated in continuous mode", () => {
  it("rejects every filler fixture", () => {
    const passed = FILLERS.filter(
      (transcript) => shouldAsk({ transcript, mode: "continuous" }).ask
    );
    assert.deepEqual(
      passed,
      [],
      `continuous mode let filler through: ${JSON.stringify(passed)}`
    );
  });

  it("explains every rejection with a reason and a status", () => {
    for (const transcript of FILLERS) {
      const decision = shouldAsk({ transcript, mode: "continuous" });
      assert.equal(decision.ask, false, transcript);
      assert.notEqual(decision.reason, "ok", transcript);
      assert.ok(decision.status.length > 0, `no status for: ${transcript}`);
    }
  });

  it("echoes what was heard so a bad gate is visible", () => {
    const decision = shouldAsk({ transcript: "hmm", mode: "continuous" });
    assert.match(decision.status, /hmm/);
    assert.match(decision.status, /filler/);
  });

  it("truncates a long echo instead of dumping the whole transcript", () => {
    const decision = shouldAsk({
      transcript: `${"yeah ".repeat(40)}`,
      mode: "continuous",
    });
    assert.equal(decision.ask, false);
    assert.ok(decision.status.length < 120, decision.status);
    assert.match(decision.status, /…/);
  });
});

describe("mode sensitivity", () => {
  it("accepts short commands in explicit mode", () => {
    const rejected = EXPLICIT_ONLY_COMMANDS.filter(
      (transcript) => !shouldAsk({ transcript, mode: "explicit" }).ask
    );
    assert.deepEqual(
      rejected,
      [],
      `explicit mode rejected legitimate commands: ${JSON.stringify(rejected)}`
    );
  });

  it("rejects the acknowledgement subset in continuous mode", () => {
    const passed = ACKNOWLEDGEMENTS_GATED_WHEN_HANDS_FREE.filter(
      (transcript) => shouldAsk({ transcript, mode: "continuous" }).ask
    );
    assert.deepEqual(
      passed,
      [],
      `continuous mode let acknowledgements through: ${JSON.stringify(passed)}`
    );
  });

  it("still rejects pure hesitation in explicit mode", () => {
    for (const transcript of ["hmm", "uh", "um...", "Hmmmm.", "er", "ahh"]) {
      assert.equal(
        shouldAsk({ transcript, mode: "explicit" }).ask,
        false,
        transcript
      );
    }
  });

  it("keeps unambiguous commands in both modes", () => {
    for (const transcript of ["cancel it", "stop", "continue", "summarize"]) {
      assert.equal(
        shouldAsk({ transcript, mode: "continuous" }).ask,
        true,
        transcript
      );
      assert.equal(
        shouldAsk({ transcript, mode: "explicit" }).ask,
        true,
        transcript
      );
    }
  });

  it("never applies the length floor in explicit mode", () => {
    assert.equal(isBelowLengthFloor("revenue", "explicit"), false);
    assert.equal(isBelowLengthFloor("revenue", "continuous"), true);
  });

  it("does not apply the length floor to two-word utterances", () => {
    for (const transcript of [
      "summarize this",
      "who won",
      "translate that",
      "action items",
      "budget forecast",
    ]) {
      assert.equal(
        shouldAsk({ transcript, mode: "continuous" }).ask,
        true,
        transcript
      );
    }
  });

  it("lets a question mark override the length floor", () => {
    assert.equal(isBelowLengthFloor("revenue?", "continuous"), false);
    assert.equal(shouldAsk({ transcript: "revenue?", mode: "continuous" }).ask, true);
  });
});

describe("reason codes", () => {
  it("reports empty for whitespace-only transcripts", () => {
    for (const transcript of ["", " ", "\n\t "]) {
      assert.equal(shouldAsk({ transcript, mode: "continuous" }).reason, "empty");
      assert.equal(shouldAsk({ transcript, mode: "explicit" }).reason, "empty");
    }
  });

  it("reports non-speech for annotations and punctuation", () => {
    for (const transcript of ["[BLANK_AUDIO]", "...", "♪", "(music)", "*laughs*", "--"]) {
      assert.equal(
        shouldAsk({ transcript, mode: "continuous" }).reason,
        "non-speech",
        transcript
      );
    }
  });

  it("reports filler for whole-utterance backchannels", () => {
    for (const transcript of ["hmm", "yeah yeah", "got it", "theek hai"]) {
      assert.equal(
        shouldAsk({ transcript, mode: "continuous" }).reason,
        "filler",
        transcript
      );
    }
  });

  it("reports too-short for lone non-command words", () => {
    for (const transcript of ["revenue", "Sarah", "Tuesday", "okay revenue"]) {
      assert.equal(
        shouldAsk({ transcript, mode: "continuous" }).reason,
        "too-short",
        transcript
      );
    }
  });
});

describe("text helpers", () => {
  it("normalizes case, punctuation and annotations", () => {
    assert.equal(normalizeTranscript("Hmm, OK!"), "hmm ok");
    assert.equal(normalizeTranscript("[BLANK_AUDIO]"), "");
    assert.equal(normalizeTranscript("uh-huh"), "uh huh");
    assert.equal(normalizeTranscript("don't"), "don't");
    assert.equal(normalizeTranscript("don\u2019t"), "don't");
    assert.equal(normalizeTranscript("समझाओ"), "समझाओ");
  });

  it("detects non-speech transcripts", () => {
    assert.equal(isNonSpeechAnnotation("..."), true);
    assert.equal(isNonSpeechAnnotation("hmm"), false);
  });

  it("tokenizes without empty tokens", () => {
    assert.deepEqual(tokenize(normalizeTranscript("  ok,  so ... what? ")), [
      "ok",
      "so",
      "what",
    ]);
  });

  it("keeps content words as residual and drops filler", () => {
    assert.deepEqual(
      residualTokens("okay so what is the plan", "continuous"),
      ["what", "is", "the", "plan"]
    );
    assert.deepEqual(residualTokens("got it thanks", "continuous"), []);
  });

  it("does not treat a phrase word as filler on its own", () => {
    // "got it" is filler; "got" alone must not consume a real utterance.
    assert.deepEqual(residualTokens("got any numbers", "continuous"), [
      "got",
      "any",
      "numbers",
    ]);
  });

  it("matches elongated spellings", () => {
    assert.equal(isBackchannel("hmmmmm", "explicit"), true);
    assert.equal(isBackchannel("okkkk", "continuous"), true);
    assert.equal(isBackchannel("yessss", "continuous"), true);
    assert.equal(isBackchannel("summarize", "continuous"), false);
  });
});
