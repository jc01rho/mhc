---
name: eval-integrity
description: "Assess integrity of an eval, benchmark, metric, A/B test, holdout, LLM-judge, or user-study claim when the question is whether measurement, comparison, or evidence is independent and trustworthy."
---

# Evaluation Integrity

Perform a read-only audit of whether an evaluation setup genuinely supports its stated claim. Never modify code, datasets, prompts, ground-truth labels, or study execution. Open the response with exactly one verdict line in the format `**Verdict: PASS**`, `**Verdict: FAIL**`, or `**Verdict: INCONCLUSIVE**`. Do not emit a second verdict line or repeat that token later in the audit. A strong headline metric is never by itself proof of evaluation integrity.

## Scope and routing

- **Implementation or security review:** route code correctness, architecture, and security audits to `review-work`.
- **Dataset computation or modeling:** route data cleaning, joins, aggregation, and statistical plotting to `data-scientist`.
- Apply this skill strictly to evaluate whether the measurement design, comparison controls, and evidence chain are independent and valid.

## Audit workflow

### 1. Formulate the claim and role map

State the claim as a falsifiable proposition: the exact comparison, metric, threshold, or causal assertion, the target population, and the decision rule. Record each evaluation role before assessing validity:

| Role | What to record |
| --- | --- |
| Claim | The conclusion and downstream decision the evaluation is used to justify. |
| System under test | Exact model, revision, configuration, prompt template, policy, or experimental arm. |
| Dataset and split | Data origins, collection window, filtering criteria, and train/dev/test or holdout partitioning. |
| Designer | The party who chose tasks, samples, prompts, arms, metrics, and stopping criteria. |
| Evaluator or judge | The human raters, judge model, prompt revision, or rubric producing quality scores. |
| Scorer | The aggregation logic or script mapping raw outputs or ratings into the headline metric. |
| Subjects | Human participants, records, tasks, or evaluation units exposed to the treatment. |

Check for overlapping authorship, shared training/tuning corpora, shared prompts, or shared selection authority across roles. Using multiple models does **not** guarantee independence; verify distinct training lineages, prompt authoring, label provenance, and decision control.

### 2. Verify independent ground truth

Locate the external reference standard capable of falsifying the claim without recycling the evaluated system's own outputs, preferences, labels, or optimization objective. Confirm that the reference standard is:

- independent of the system under test and its authors;
- gathered or adjudicated blind to treatment assignment whenever blinding is possible;
- directly aligned with the claim rather than a proxy chosen because it flatters one arm; and
- locked prior to comparing arms, or paired with a transparent multiple-testing and selection disclosure.

When no independent reference standard exists, agreement among correlated evaluators cannot replace it. Issue **INCONCLUSIVE** unless the claim is explicitly scoped to agreement with that specific evaluator.

### 3. Check failure modes in independence

Inspect only the failure modes applicable to the mapped design:

- **Benchmark contamination:** test items, reference answers, or near-duplicates leaked into pretraining, fine-tuning, prompt examples, retrieval stores, or post-hoc filtering.
- **Confounded controls:** experimental arms differ in variables outside the treatment, assignment is skewed, effort or budget is asymmetric, or the baseline cannot isolate the causal effect.
- **Circular evaluation:** the system under test, its designer, or a rubric derived from its behavior defines success without external validation.
- **Scorer-label leakage:** thresholds, reward weights, or rubric terms are shared between optimization and scoring so high scores are tautological.
- **Evaluator-designer coupling:** judge prompts, rater instructions, or tie-breaking rules are tuned by the team advocating for one arm without independent oversight.
- **Split or rater correlation:** duplicate records across splits, temporal lookahead, shared annotator pools, or correlated labeling guidelines break split independence.
- **Hypothesis priming:** instructions, examples, unblinded previews, or interim readouts nudge raters or analysts toward the desired outcome.
- **Observer and demand artifacts:** participants or raters deduce the favored arm and alter behavior or scoring accordingly.

Separate verified provenance from assumptions. Unrecorded provenance for datasets, splits, labels, prompts, judge selection, or aggregation rules yields **INCONCLUSIVE**, never an assumed pass.

### 4. Determine the verdict

- **PASS:** documented evidence establishes an independent, valid measurement chain for the stated claim with no material unresolved coupling.
- **FAIL:** concrete evidence demonstrates contamination, circularity, or a confounded control that invalidates or overstates the claim.
- **INCONCLUSIVE:** key provenance is absent, independent ground truth is unverified, or open uncertainty prevents a definitive pass or fail.

Never upgrade uncertainty to PASS on the strength of plausible numbers, p-values, or multi-judge consensus.

## Output structure

Lead with the single verdict line. Follow with only the findings that triggered; omit boilerplate checklists of risks that did not fire. For each triggered finding, state:

1. **Evidence:** the concrete artifact, provenance record, or missing link observed.
2. **Impact:** which part of the claim or causal inference is undermined.
3. **Minimal independence fix:** the smallest structural change (blinding, held-out split, external adjudication, equalized control) that resolves the coupling.
4. **Missing evidence:** the specific provenance or measurement needed when the finding is inconclusive.
5. **Residual risk:** what uncertainty remains even after applying the minimal fix.

Keep every conclusion strictly within the observed evidence. This audit advises on measurement validity and never certifies a release or recomputes metrics.
