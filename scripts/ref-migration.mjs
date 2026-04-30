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

const meshes = run(`git for-each-ref --format='%(refname)' refs/meshes/v1/`).split('\n').filter(Boolean);

for (const mesh of meshes) {
    const meshName = mesh.replace('refs/meshes/v1/', '');
    console.log(`Migrating mesh ${meshName}...`);
    const commitId = run(`git rev-parse ${mesh}`);
    const lsTreeAnchors = run(`git ls-tree ${commitId} anchors`);
    if (!lsTreeAnchors) {
        const lsTreeAnchorsV2 = run(`git ls-tree ${commitId} anchors.v2`);
        if (lsTreeAnchorsV2) {
             console.log(`Mesh ${meshName} already has anchors.v2`);
        }
        continue;
    }
    const anchorsBlobId = lsTreeAnchors.split(/\s+/)[2];
    const anchorIds = run(`git cat-file -p ${anchorsBlobId}`).split('\n').filter(Boolean);
    let anchorsV2Content = '';
    for (const id of anchorIds) {
        const anchorRef = `refs/anchors/v1/${id}`;
        try {
            const anchorBlobId = run(`git rev-parse ${anchorRef}`);
            const anchorContent = run(`git cat-file -p ${anchorBlobId}`);
            anchorsV2Content += `id ${id}\n${anchorContent}`;
            if (!anchorsV2Content.endsWith('\n')) {
                anchorsV2Content += '\n';
            }
            anchorsV2Content += '\n';
        } catch (e) {
            console.warn(`Could not find anchor ref ${anchorRef}`);
        }
    }
    
    // Write anchors.v2
    const newAnchorsV2Blob = run(`git hash-object -w --stdin`, anchorsV2Content);
    const lsTreeConfig = run(`git ls-tree ${commitId} config`);
    const configBlobId = lsTreeConfig ? lsTreeConfig.split(/\s+/)[2] : null;
    
    let mktreeInput = `100644 blob ${newAnchorsV2Blob}\tanchors.v2\n`;
    if (configBlobId) {
        mktreeInput += `100644 blob ${configBlobId}\tconfig\n`;
    }
    
    const newTree = run(`git mktree`, mktreeInput);
    const message = run(`git log -1 --format=%B ${commitId}`);
    
    // Get parents (skip if none)
    const parents = run(`git log -1 --format=%P ${commitId}`).split(' ').filter(Boolean).map(p => `-p ${p}`).join(' ');
    
    if (dryRun) {
        console.log(`[Dry Run] Would update ${mesh} to tree ${newTree}`);
    } else {
        const messagePath = '/tmp/mesh_message.txt';
        execSync(`cat > ${messagePath}`, { input: message });
        const newCommit = run(`git commit-tree ${newTree} ${parents} -F ${messagePath}`);
        run(`git update-ref ${mesh} ${newCommit} ${commitId}`);
        console.log(`Updated ${mesh}`);
    }
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
    console.log('No anchor refs found to delete.');
}
