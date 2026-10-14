/**
 * Layer 1 of the ask-gating ladder: a pure-text backchannel filter.
 *
 * Pure functions only — no React, no Tauri, no I/O, no dependencies. Every
 * function here is synchronous and sub-millisecond, so it is safe to run on the
 * hot path between STT and a billed LLM call.
 *
 * Design bias: FALSE NEGATIVES (rejecting a real request) ARE FAR WORSE THAN
 * FALSE POSITIVES (letting filler through). Two rules enforce that bias:
 *
 *  1. An utterance is only treated as a backchannel when *every* token is a
 *     known filler. A single unrecognised word makes it pass. That means the
 *     filler lists can never swallow "okay so what should we do" — "so",
 *     "what", "should", "we", "do" are not all fillers.
 *  2. The vocabulary is tiered by capture mode. "yes", "no", "cancel it" are
 *     legitimate commands when the user deliberately opened the mic, so only
 *     non-lexical hesitation ("hmm", "uh") is rejected in explicit mode.
 */

/**
 * Which capture mode produced the transcript.
 *
 * - `continuous` — hands-free capture. The energy VAD segments whatever it
 *   hears, most of which is not addressed to the assistant. Strict gate.
 * - `explicit` — the user deliberately started the recording (press-to-record
 *   or the mic button). Intent is already established. Lenient gate: only
 *   meaningless hesitation is dropped.
 *
 * NOTE on mapping this from `useSystemAudio`: the hook's `isContinuousMode`
 * flag is the *manual press-to-record* mode (`vadConfig.enabled === false`,
 * labelled "Manual" in the UI). The hands-free mode is `isContinuousMode ===
 * false`. The names are inverted relative to this enum, so the call site maps
 * `isContinuousMode ? "explicit" : "continuous"`.
 */
export type GateMode = "continuous" | "explicit";

/**
 * Non-lexical hesitation. Carries no meaning in any mode, so these are
 * rejected even when the user explicitly opened the mic.
 */
export const HESITATION_FILLERS: readonly string[] = [
  "hm",
  "hmm",
  "hmmm",
  "mm",
  "mmm",
  "mmh",
  "uh",
  "uhh",
  "um",
  "umm",
  "uhm",
  "er",
  "err",
  "erm",
  "ah",
  "aah",
  "eh",
  "ehh",
  "oh",
  "ohh",
  "ooh",
  "ugh",
  "oof",
  "aw",
  "aww",
  "phew",
  "shh",
  "tsk",
];

/**
 * Acknowledgements, discourse markers and short social replies. Meaningful
 * when the user deliberately opened the mic ("yes", "no", "go on"), noise when
 * we are listening to a room. Rejected in `continuous` mode only.
 */
export const ACKNOWLEDGEMENT_FILLERS: readonly string[] = [
  // affirmatives
  "ok",
  "okay",
  "okey",
  "oke",
  "k",
  "kk",
  "yeah",
  "yea",
  "yeh",
  "ya",
  "yah",
  "yup",
  "yep",
  "yes",
  "aye",
  "mhm",
  "mmhm",
  "huh",
  // negatives
  "no",
  "nope",
  "nah",
  // assessments
  "right",
  "alright",
  "sure",
  "cool",
  "nice",
  "great",
  "awesome",
  "perfect",
  "exactly",
  "correct",
  "true",
  "agreed",
  "fine",
  "indeed",
  "totally",
  "absolutely",
  "definitely",
  "certainly",
  "obviously",
  "wow",
  "oops",
  // discourse markers
  "well",
  "so",
  "like",
  "anyway",
  "anyways",
  "actually",
  "basically",
  "literally",
  "honestly",
  "seriously",
  "whatever",
  // social
  "thanks",
  "thankyou",
  "cheers",
  "please",
  "sorry",
  // Hinglish
  "haan",
  "han",
  "haa",
  "ha",
  "accha",
  "acha",
  "achha",
  "theek",
  "thik",
  "bas",
  "matlab",
  "arre",
  "arey",
  "bilkul",
  "sahi",
  "ji",
  "nahi",
  "nahin",
  "na",
];

/**
 * Multi-word acknowledgements. Kept as phrases rather than adding their words
 * to {@link ACKNOWLEDGEMENT_FILLERS} so that "got" alone never counts as
 * filler. Matched as exact whole-token sequences. Rejected in `continuous`
 * mode only.
 */
export const ACKNOWLEDGEMENT_PHRASES: readonly string[] = [
  "got it",
  "gotcha",
  "i see",
  "i know",
  "i understand",
  "i agree",
  "i mean",
  "you know",
  "makes sense",
  "that makes sense",
  "sounds good",
  "sounds great",
  "fair enough",
  "no worries",
  "no problem",
  "not a problem",
  "never mind",
  "nevermind",
  "all right",
  "uh huh",
  "mm hmm",
  "kind of",
  "sort of",
  "of course",
  "for sure",
  "thank you",
  "thanks a lot",
  "thank you so much",
  "many thanks",
  "my bad",
  "go on",
  "carry on",
  "that's right",
  "thats right",
  "that's true",
  "thats true",
  "good point",
  "well said",
  "very good",
  "ha ha",
  "he he",
  "theek hai",
  "thik hai",
  "ji haan",
];

/**
 * Single words that are real requests on their own. These survive the
 * one-word length floor. Deliberately generous: a word missing from this list
 * is a false negative, which is the expensive kind of mistake.
 */
export const SHORT_COMMAND_ALLOWLIST: readonly string[] = [
  // interrogatives
  "what",
  "whats",
  "why",
  "how",
  "who",
  "whom",
  "whose",
  "where",
  "when",
  "which",
  "whether",
  // ask again / shape the last answer
  "summarize",
  "summarise",
  "summary",
  "tldr",
  "recap",
  "explain",
  "elaborate",
  "expand",
  "clarify",
  "define",
  "describe",
  "translate",
  "rephrase",
  "paraphrase",
  "simplify",
  "continue",
  "proceed",
  "next",
  "repeat",
  "again",
  "retry",
  "redo",
  "undo",
  "stop",
  "cancel",
  "pause",
  "resume",
  "list",
  "compare",
  "calculate",
  "convert",
  "solve",
  "code",
  "debug",
  "fix",
  "refactor",
  "optimize",
  "write",
  "draft",
  "rewrite",
  "edit",
  "review",
  // asking for specific content
  "source",
  "sources",
  "cite",
  "citation",
  "example",
  "examples",
  "detail",
  "details",
  "more",
  "less",
  "shorter",
  "longer",
  "louder",
  "slower",
  "faster",
  "help",
  "hint",
  "answer",
  "options",
  "pros",
  "cons",
  "steps",
  "agenda",
  "notes",
  "actions",
  "followups",
  "wait",
  // Romanised Hindi imperatives. Without these the floor silently eats real
  // one-word requests ("batao", "samjhao") in exactly the languages the filler
  // list already covers.
  "batao",
  "bataao",
  "bataiye",
  "bataye",
  "samjhao",
  "samjhaiye",
  "likho",
  "likhiye",
  "karo",
  "kijiye",
  "kariye",
  "dikhao",
  "dikhaiye",
  "sunao",
  "sunaiye",
  "padho",
  "padhiye",
  "dhundo",
  "khojo",
  "banao",
  "banaiye",
  "bhejo",
  "bhejiye",
  "hatao",
  "badlo",
  "ruko",
  "rukiye",
];

/**
 * Bracketed / starred / musical annotations that STT engines emit for
 * non-speech audio: `[BLANK_AUDIO]`, `(music)`, `*laughs*`, `♪`.
 */
const ANNOTATION_PATTERN = /[[({<][^\])}>]*[\])}>]|\*[^*]*\*|♪[^♪]*♪|♪+/gu;

/**
 * Anything that is not a letter, combining mark, digit, apostrophe or
 * whitespace. `\p{M}` is required: Devanagari vowel signs (and Latin combining
 * diacritics) are marks, not letters, so omitting it shreds Hindi/Marathi/
 * Bengali transcripts — "समझाओ" would become "समझ ओ".
 */
const NON_WORD_PATTERN = /[^\p{L}\p{M}\p{N}'\s]+/gu;

/** A run of the same letter repeated two or more times. */
const ELONGATION_PATTERN = /(\p{L})\1+/gu;

/**
 * Collapse elongated spellings so "hmmmm", "okkk" and "yesss" match their
 * canonical entries. Runs of a repeated letter collapse to a single letter.
 */
export function collapseElongation(token: string): string {
  return token.replace(ELONGATION_PATTERN, "$1");
}

/**
 * Lowercase, drop non-speech annotations, and reduce punctuation to
 * whitespace. Apostrophes survive so "don't" stays one token. Unicode-aware,
 * so Devanagari and other non-Latin scripts pass through intact.
 */
export function normalizeTranscript(raw: string): string {
  return raw
    .toLowerCase()
    .replace(/[\u2018\u2019\u02bc]/gu, "'")
    .replace(ANNOTATION_PATTERN, " ")
    .replace(NON_WORD_PATTERN, " ")
    .replace(/\s+/gu, " ")
    .trim();
}

/** Split a normalized transcript into tokens, trimming edge apostrophes. */
export function tokenize(normalized: string): string[] {
  if (!normalized) return [];
  return normalized
    .split(" ")
    .map((token) => token.replace(/^'+|'+$/gu, ""))
    .filter((token) => token.length > 0);
}

/**
 * True when the transcript contains no speech at all — only punctuation or
 * non-speech annotations. `"..."`, `"[BLANK_AUDIO]"` and `"♪"` all qualify.
 */
export function isNonSpeechAnnotation(raw: string): boolean {
  return normalizeTranscript(raw).length === 0;
}

/** Build a lookup set that also contains the elongation-collapsed spellings. */
function buildLookup(entries: readonly (readonly string[])[]): Set<string> {
  const lookup = new Set<string>();
  for (const group of entries) {
    for (const entry of group) {
      lookup.add(entry);
      lookup.add(collapseElongation(entry));
    }
  }
  return lookup;
}

const HESITATION_LOOKUP = buildLookup([HESITATION_FILLERS]);
const CONTINUOUS_LOOKUP = buildLookup([
  HESITATION_FILLERS,
  ACKNOWLEDGEMENT_FILLERS,
]);
const PHRASE_LOOKUP = new Set<string>(ACKNOWLEDGEMENT_PHRASES);
const COMMAND_LOOKUP = buildLookup([SHORT_COMMAND_ALLOWLIST]);
const EMPTY_LOOKUP = new Set<string>();
const MAX_PHRASE_TOKENS = ACKNOWLEDGEMENT_PHRASES.reduce(
  (max, phrase) => Math.max(max, phrase.split(" ").length),
  0
);

function fillerTokens(mode: GateMode): ReadonlySet<string> {
  return mode === "explicit" ? HESITATION_LOOKUP : CONTINUOUS_LOOKUP;
}

function fillerPhrases(mode: GateMode): ReadonlySet<string> {
  return mode === "explicit" ? EMPTY_LOOKUP : PHRASE_LOOKUP;
}

/** True when a single token is filler in the given mode. */
export function isFillerToken(token: string, mode: GateMode): boolean {
  const lookup = fillerTokens(mode);
  return lookup.has(token) || lookup.has(collapseElongation(token));
}

/** True when a single token is a recognised one-word request. */
export function isShortCommand(token: string): boolean {
  return (
    COMMAND_LOOKUP.has(token) || COMMAND_LOOKUP.has(collapseElongation(token))
  );
}

/**
 * The tokens left after removing every filler phrase and filler token. An
 * empty result means the utterance was nothing but backchannel.
 *
 * Phrases are matched greedily longest-first at each position so that
 * "got it" is consumed as one unit and "got" never counts as filler alone.
 */
export function residualTokens(raw: string, mode: GateMode): string[] {
  const tokens = tokenize(normalizeTranscript(raw));
  const phrases = fillerPhrases(mode);
  const residual: string[] = [];

  let index = 0;
  while (index < tokens.length) {
    let consumed = 0;
    const maxSpan = Math.min(MAX_PHRASE_TOKENS, tokens.length - index);
    for (let span = maxSpan; span >= 2; span--) {
      if (phrases.has(tokens.slice(index, index + span).join(" "))) {
        consumed = span;
        break;
      }
    }
    if (consumed > 0) {
      index += consumed;
      continue;
    }
    if (!isFillerToken(tokens[index], mode)) {
      residual.push(tokens[index]);
    }
    index += 1;
  }

  return residual;
}

/** True when the utterance is entirely backchannel for the given mode. */
export function isBackchannel(raw: string, mode: GateMode): boolean {
  return (
    !isNonSpeechAnnotation(raw) && residualTokens(raw, mode).length === 0
  );
}

/**
 * The length floor. Applies to hands-free capture only, rejects only
 * single-word utterances, and steps aside for anything that looks like a
 * question or a recognised one-word command.
 *
 * This is the loosest rule in the module and the one most likely to cause a
 * false negative, which is why it is scoped this narrowly. Two-word
 * utterances ("summarize this", "who won") are never touched.
 */
export function isBelowLengthFloor(
  raw: string,
  mode: GateMode,
  residual: string[] = residualTokens(raw, mode)
): boolean {
  if (mode !== "continuous") return false;
  if (residual.length !== 1) return false;
  if (raw.includes("?")) return false;
  return !isShortCommand(residual[0]);
}
