/** Independent, offline instruction oracle; input contains only public keys and unsigned instructions. */
const path=require('node:path'),fs=require('node:fs'),assert=require('node:assert/strict');
const modules=process.env.ORACLE_NODE_MODULES||path.join(__dirname,'node_modules');
const load=n=>require(path.join(modules,n));
const pump=load('@pump-fun/pump-sdk'),{PublicKey}=load('@solana/web3.js'),BN=load('bn.js');
assert.equal(load('@pump-fun/pump-sdk/package.json').version,'2.0.0');
(async()=>{
 const cases=JSON.parse(fs.readFileSync(0,'utf8'));
 for(const c of cases){
  const p=Object.fromEntries(['user','mint','creator','feeRecipient','buybackFeeRecipient','tokenProgram','quoteMint'].map(k=>[k,new PublicKey(c[k])]));
  p.amount=new BN(c.sell?10_000_000:1_000);p.solAmount=new BN(c.sell?1_000:10_000_000);p.quoteAmount=p.solAmount;p.cashback=c.cashback;
  const sdk=pump.PUMP_SDK;
  const ix=await (c.v2?(c.sell?sdk.getSellV2InstructionRaw(p):sdk.getBuyV2InstructionRaw(p)):(c.sell?sdk.getSellInstructionRaw(p):sdk.getBuyInstructionRaw(p)));
  if(!c.v2&&!c.sell) ix.data[24]=Number(c.cashback); // Rust policy selects the optional tracking flag from cashback.
  const label=JSON.stringify({v2:c.v2,sell:c.sell,cashback:c.cashback,tokenProgram:c.tokenProgram});
  assert.deepEqual(c.data,[...ix.data],label+' instruction bytes');
  assert.deepEqual(c.accounts,ix.keys.map(m=>[m.pubkey.toBase58(),m.isSigner,m.isWritable]),label+' accounts');
 }
 console.log(JSON.stringify({officialSdk:'@pump-fun/pump-sdk@2.0.0',instructionCases:cases.length,status:'passed'}));
})().catch(e=>{console.error(e);process.exitCode=1;});
