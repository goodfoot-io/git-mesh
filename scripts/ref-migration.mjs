#!/usr/bin/env node
import { execSync } from 'child_process';
import { parseArgs } from 'util';

const { values: { 'dry-run': dryRun } } = parseArgs({
  options: {
    'dry-run': { type: 'boolean' }
  }
});

function run(cmd, input) {
    return execSync(cmd, { input, encoding: 'utf8' }).trim();
}

function tryRun(cmd, input) {
    try {
        return run(cmd, input);
    } catch {
        return null;
    }
}

function isEmbeddedAnchorsContent(content) {
    const firstLine = content.split('\n').find(l => l.length > 0);
    return !!firstLine && firstLine.startsWith('id ');
}

function buildEmbeddedFromLegacyIds(meshName, ids) {
    let out = '';
    for (const id of ids) {
        const anchorRef = `refs/anchors/v1/${id}`;
        const anchorBlobId = tryRun(`git rev-parse ${anchorRef}`);
        if (!anchorBlobId) {
            console.warn(`[${meshName}] Could not find anchor ref ${anchorRef}`);
            continue;
        }
        const anchorContent = run(`git cat-file -p ${anchorBlobId}`);
        out += `id ${id}\n${anchorContent}`;
        if (!out.endsWith('\n')) out += '\n';
        out += '\n';
    }
    return out;
}

function rewriteMesh(meshRef, newAnchorsBlob, configBlobId, originalCommitId) {
    let mktreeInput = `100644 blob ${newAnchorsBlob}\tanchors\n`;
    if (configBlobId) {
        mktreeInput += `100644 blob ${configBlobId}\tconfig\n`;
    }
    const newTree = run(`git mktree`, mktreeInput);
    const message = run(`git log -1 --format=%B ${originalCommitId}`);
    const parents = run(`git log -1 --format=%P ${originalCommitId}`)
        .split(' ').filter(Boolean).map(p => `-p ${p}`).join(' ');

    if (dryRun) {
        console.log(`[Dry Run] Would update ${meshRef} to tree ${newTree}`);
        return;
    }
    const messagePath = '/tmp/mesh_message.txt';
    execSync(`cat > ${messagePath}`, { input: message });
    const newCommit = run(`git commit-tree ${newTree} ${parents} -F ${messagePath}`);
    run(`git update-ref ${meshRef} ${newCommit} ${originalCommitId}`);
    console.log(`Updated ${meshRef}`);
}

const meshes = run(`git for-each-ref --format='%(refname)' refs/meshes/v1/`).split('\n').filter(Boolean);

for (const mesh of meshes) {
    const meshName = mesh.replace('refs/meshes/v1/', '');
    console.log(`Inspecting mesh ${meshName}...`);
    const commitId = run(`git rev-parse ${mesh}`);

    const lsTreeAnchorsV2 = tryRun(`git ls-tree ${commitId} anchors.v2`);
    const lsTreeAnchors = tryRun(`git ls-tree ${commitId} anchors`);
    const lsTreeConfig = tryRun(`git ls-tree ${commitId} config`);
    const configBlobId = lsTreeConfig ? lsTreeConfig.split(/\s+/)[2] : null;

    // Case A: 1.0.35/1.0.36 layout — `anchors.v2` exists. Rename to `anchors`.
    if (lsTreeAnchorsV2) {
        const anchorsV2BlobId = lsTreeAnchorsV2.split(/\s+/)[2];
        if (lsTreeAnchors) {
            console.warn(`[${meshName}] Both anchors and anchors.v2 present; preferring anchors.v2 and dropping anchors.`);
        }
        console.log(`[${meshName}] Renaming anchors.v2 -> anchors`);
        rewriteMesh(mesh, anchorsV2BlobId, configBlobId, commitId);
        continue;
    }

    // No anchors.v2. Inspect `anchors` to decide if it's legacy IDs or already embedded.
    if (!lsTreeAnchors) {
        console.log(`[${meshName}] No anchors blob found; skipping`);
        continue;
    }

    const anchorsBlobId = lsTreeAnchors.split(/\s+/)[2];
    const anchorsContent = run(`git cat-file -p ${anchorsBlobId}`);

    if (isEmbeddedAnchorsContent(anchorsContent)) {
        console.log(`[${meshName}] Already on 1.0.37 anchors layout`);
        continue;
    }

    // Case B: 1.0.34 legacy — `anchors` blob is a list of IDs pointing to refs/anchors/v1/<id>.
    const anchorIds = anchorsContent.split('\n').filter(Boolean);
    const embedded = buildEmbeddedFromLegacyIds(meshName, anchorIds);
    const newAnchorsBlob = run(`git hash-object -w --stdin`, embedded);
    console.log(`[${meshName}] Migrating legacy anchors -> embedded anchors`);
    rewriteMesh(mesh, newAnchorsBlob, configBlobId, commitId);
}

const anchorRefs = run(`git for-each-ref --format='%(refname)' refs/anchors/v1/`).split('\n').filter(Boolean);
if (anchorRefs.length > 0) {
    if (dryRun) {
        console.log(`[Dry Run] Would delete ${anchorRefs.length} anchor refs`);
    } else {
        const deleteInput = anchorRefs.map(ref => `delete ${ref}`).join('\n');
        run(`git update-ref --stdin`, deleteInput + '\n');
        console.log(`Deleted ${anchorRefs.length} anchor refs`);
    }
} else {
    console.log('No legacy refs/anchors/v1 refs found.');
}
