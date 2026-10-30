import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ProgressConflictError, ProgressCoordinator } from '../src/core/api/progressCoordinator.ts';
import type { ProgressConflict, ProgressSnapshot } from '../src/core/api/progressCoordinator.ts';

test('concurrent update between read and write preserves the local draft until explicit resolution', async () => {
  let remote: ProgressSnapshot={book_id:'book',chapter_id:'one',position:10,revision:1};
  let collide=true;const conflicts:ProgressConflict[]=[];const revisions:number[]=[];
  const sync=new ProgressCoordinator({read:async()=>({...remote}),write:async(value,revision)=>{
    revisions.push(revision!);
    if(collide) {collide=false;remote={...remote,position:90,revision:2};throw new ProgressConflictError(remote);}
    assert.equal(revision,remote.revision);remote={...value,revision:remote.revision!+1};return remote;
  }},()=>'owner',value=>conflicts.push(value));
  await sync.save({book_id:'book',chapter_id:'one',position:20});
  assert.equal(sync.hasConflict('book'),true);assert.equal(conflicts[0].local.position,20);
  await sync.save({book_id:'book',chapter_id:'one',position:21});
  assert.deepEqual(revisions,[1]);
  await sync.resolve('book',false);
  assert.deepEqual(revisions,[1,2]);assert.equal(remote.position,21);assert.equal(sync.hasConflict('book'),false);
});

test('writes coalesce while a request is active and never cross an account change', async () => {
  let release!:()=>void;const gate=new Promise<void>(resolve=>{release=resolve;});
  let reads=0;let owner='one';const saved:number[]=[];
  const sync=new ProgressCoordinator({read:async()=>{if(++reads===1) await gate;return {book_id:'book',position:0,revision:0};},
    write:async value=>{saved.push(value.position);return {...value,revision:1};}},()=>owner,()=>{});
  const first=sync.save({book_id:'book',chapter_id:'one',position:10});
  void sync.save({book_id:'book',chapter_id:'one',position:20});
  release();await first;assert.deepEqual(saved,[20]);
  const changing=new ProgressCoordinator({read:async()=>{owner='other';return {book_id:'book',position:0,revision:0};},
    write:async value=>{saved.push(value.position);return value;}},()=>owner,()=>{});
  await changing.save({book_id:'book',position:30});assert.deepEqual(saved,[20]);
});

test('accepting the server snapshot does not upload the discarded local position', async()=>{
  let remote:ProgressSnapshot={book_id:'book',chapter_id:'one',position:1,revision:0};let writes=0;
  const sync=new ProgressCoordinator({read:async()=>({...remote}),write:async value=>{writes++;if(writes===1) {remote={...remote,position:50,revision:1};throw new ProgressConflictError(remote);}return value;}},()=>'owner',()=>{});
  await sync.save({book_id:'book',chapter_id:'one',position:20});
  const selected=await sync.resolve('book',true);assert.equal(selected?.position,50);assert.equal(writes,1);
});
