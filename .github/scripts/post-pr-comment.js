// Posts/updates the sticky cargo-crap comment, invoked from pr-comment.yml via
// actions/github-script. The artifact body is untrusted (fork-controllable) and
// used only as the comment text; the target PR comes from the trusted event.
module.exports = async ({ github, context, core }) => {
  const fs = require('fs');
  // Fixed path only — never anything else the untrusted artifact carried.
  const commentFile = 'pr-artifact/crap-comment.md';
  if (!fs.existsSync(commentFile)) return;
  const marker = '<!-- cargo-crap-report -->';
  let body = fs.readFileSync(commentFile, 'utf8');
  // The body is fork-controllable: force the sticky marker onto it, or a
  // marker-less body updating the existing comment would destroy the marker
  // and orphan the comment for every later run.
  if (!body.startsWith(marker)) body = `${marker}\n${body}`;

  // Resolve the target PR from the trusted event only. Require an open PR
  // whose current head is exactly the reviewed sha: pull_requests may list
  // stale or closed PRs first (reused branch names), and a head mismatch
  // means the PR advanced past this run — a newer run posts fresh results.
  const run = context.payload.workflow_run;
  let pr = null;
  for (const cand of run.pull_requests || []) {
    const { data } = await github.rest.pulls.get({
      owner: context.repo.owner,
      repo: context.repo.repo,
      pull_number: cand.number,
    });
    if (data.state === 'open' && data.head.sha === run.head_sha) {
      pr = data;
      break;
    }
  }
  if (!pr) {
    // Fork PRs don't populate pull_requests; look up by head SHA instead
    // (still trusted, not from the artifact).
    const { data: prs } =
      await github.rest.repos.listPullRequestsAssociatedWithCommit({
        owner: context.repo.owner,
        repo: context.repo.repo,
        commit_sha: run.head_sha,
      });
    const open = prs.filter(
      (p) => p.state === 'open' && p.head.sha === run.head_sha,
    );
    if (open.length > 1) {
      core.info(`PRs #${open.map((p) => p.number).join(', #')} share head ${run.head_sha}; picking the first.`);
    }
    pr = open[0] ?? null;
  }
  if (!pr) {
    core.info('No open PR at this head SHA — stale run or closed PR; skipping comment.');
    return;
  }

  // Paginate — the marker may be past the first page; missing it dupes instead
  // of updating.
  const comments = await github.paginate(github.rest.issues.listComments, {
    owner: context.repo.owner,
    repo: context.repo.repo,
    issue_number: pr.number,
    per_page: 100,
  });
  const existing = comments.find((c) => (c.body || '').startsWith(marker));
  if (existing) {
    await github.rest.issues.updateComment({
      owner: context.repo.owner,
      repo: context.repo.repo,
      comment_id: existing.id,
      body,
    });
  } else {
    await github.rest.issues.createComment({
      owner: context.repo.owner,
      repo: context.repo.repo,
      issue_number: pr.number,
      body,
    });
  }
};
