#!/usr/bin/env python3
"""Experimental bounded codec for owned blank-paint-layer document.bin fixtures.

Clean-room observations only. Not an SPP importer/writer: container checksums,
resources, mesh, nonempty tags and other property types remain unresolved.
The CLI only compares reencoded dataset bytes and never writes an SPP file.
"""
import struct,sys,json,math
from pathlib import Path
class Reader:
 def __init__(self,b):
  if len(b)>65536 or len(b)<24 or b[:4]!=bytes.fromhex('dd2f7c1b'):raise ValueError('header/size')
  if struct.unpack_from('<I',b,4)[0]!=1 or struct.unpack_from('<I',b,20)[0]!=len(b):raise ValueError('version/declared size')
  self.b=b;self.p=20;self.classes=[];self.objects=0
 def take(self,n):
  if n<0 or self.p+n>len(self.b):raise ValueError(f'truncated at {self.p:x}')
  p=self.p;self.p+=n;return self.b[p:self.p]
 def u32(self):return struct.unpack('<I',self.take(4))[0]
 def count(self,cap=4096):
  n=self.u32()
  if n>cap:raise ValueError('count cap')
  return n
 def text(self,cap=4096):return self.take(self.count(cap)).decode('utf8')
 def definition(self):
  name=self.text(128);props=[(self.text(128),self.u32()) for _ in range(self.count(128))]
  if len({key for key,_ in props})!=len(props):raise ValueError('duplicate property')
  if any(existing[0]==name for existing in self.classes):raise ValueError('duplicate class name')
  if len(self.classes)>=256:raise ValueError('class cap')
  self.classes.append((name,props));return len(self.classes)-1
 def object(self,depth=0,pointer=True):
  self.objects+=1
  if depth>32 or self.objects>4096:raise ValueError('depth/object cap')
  start=self.p;end=None
  if pointer:
   end=self.u32()
   if end==0:return None
   if end<self.p or end>len(self.b):raise ValueError(f'bad end {end:x} at {start:x}')
  cid=self.u32()
  if cid==0xffffffff:cid=self.definition()
  if cid>=len(self.classes):raise ValueError(f'class {cid} at {start:x}')
  name,props=self.classes[cid];data={}
  for prop,kind in props:
   pos=self.p
   try:data[prop]=self.value(kind,depth+1)
   except Exception as e:raise ValueError(f'{name}.{prop} type{kind} offset{pos:x}: {e}') from e
  if end is not None and self.p!=end:raise ValueError(f'{name} end mismatch got{self.p:x} expected{end:x}')
  return {'class':name,'start':start,'end':self.p,'fields':data}
 def value(self,t,d):
  if t==1:
   v=struct.unpack('<f',self.take(4))[0]
   if not math.isfinite(v):raise ValueError('float')
   return v
  if t in (4,22):return self.take(16).hex()
  if t==6:return [self.u32(),self.u32()]
  if t==9:return self.u32()
  if t==10:
   v=self.take(1)[0]
   if v not in (0,255):raise ValueError(f'bool {v}')
   return v==255
  if t in (12,15,21):return struct.unpack('<Q',self.take(8))[0]
  if t==16:return self.text()
  if t==17:return [self.object(d,False) for _ in range(self.count())]
  if t in (18,20):return self.object(d,True)
  if t==19:return [self.object(d,True) for _ in range(self.count())]
  if t==11:
   n=self.u32()
   if n:raise ValueError(f'unresolved nonempty type{t} ({n})')
   return []
  raise ValueError(f'unresolved type{t}')

class Writer:
 def __init__(self,header,classes):self.b=bytearray(header[:20]);self.classes=classes;self.seen=[]
 def u32(self,v):self.b.extend(struct.pack('<I',v))
 def text(self,v):
  b=v.encode('utf8');self.u32(len(b));self.b.extend(b)
 def object(self,obj,pointer=True):
  start=len(self.b)
  if pointer:
   self.u32(0)
   if obj is None:return
  name=obj['class'];cid=next(i for i,c in enumerate(self.classes) if c[0]==name)
  if cid not in self.seen:
   assert cid==len(self.seen)
   self.seen.append(cid);self.u32(0xffffffff);self.text(name);self.u32(len(self.classes[cid][1]))
   for key,t in self.classes[cid][1]:self.text(key);self.u32(t)
  else:self.u32(cid)
  for key,t in self.classes[cid][1]:self.value(t,obj['fields'][key])
  if pointer:struct.pack_into('<I',self.b,start,len(self.b))
 def value(self,t,v):
  if t==1:self.b.extend(struct.pack('<f',v))
  elif t in (4,22):
   raw=bytes.fromhex(v)
   if len(raw)!=16:raise ValueError('invalid opaque16 size')
   self.b.extend(raw)
  elif t==6:
   for n in v:self.u32(n)
  elif t==9:self.u32(v)
  elif t==10:self.b.append(255 if v else 0)
  elif t in (12,15,21):self.b.extend(struct.pack('<Q',v))
  elif t==16:self.text(v)
  elif t in (17,19):
   self.u32(len(v))
   for item in v:self.object(item,t==19)
  elif t in (18,20):self.object(v)
  elif t==11:
   assert not v;self.u32(0)
  else:raise ValueError(t)
if __name__=='__main__':
 for path in sys.argv[1:]:
  with Path(path).open('rb') as f:b=f.read(65537)
  r=Reader(b);obj=r.object()
  if r.p!=len(b) or len(r.classes)!=struct.unpack_from('<I',b,16)[0]:raise ValueError('trailing bytes/class count')
  w=Writer(b,r.classes);w.object(obj)
  assert bytes(w.b)==b, path
  print(Path(path).name, 'exact dataset encode/decode',len(b),'bytes')
