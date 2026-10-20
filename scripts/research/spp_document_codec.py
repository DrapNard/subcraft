#!/usr/bin/env python3
"""Experimental bounded codec for owned Painter document.bin fixtures.

Clean-room observations only. Not an SPP importer/writer: container checksums,
resources, mesh and unknown property types remain unresolved.
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
 def class_ref(self):
  cid=self.u32()
  if cid==0xffffffff: return self.definition()
  if cid>=len(self.classes):raise ValueError(f'class {cid} at {self.p:x}')
  return cid
 def fields(self,cid,depth=0):
  if depth>32:raise ValueError('depth cap')
  self.objects+=1
  if self.objects>4096:raise ValueError('object cap')
  name,props=self.classes[cid];data={}
  for prop,kind in props:
   pos=self.p
   try:data[prop]=self.value(kind,depth+1)
   except Exception as e:raise ValueError(f'{name}.{prop} type{kind} offset{pos:x}: {e}') from e
  return {'class':name,'fields':data}
 def object(self,depth=0,pointer=True):
  self.objects+=1
  if depth>32 or self.objects>4096:raise ValueError('depth/object cap')
  start=self.p;end=None
  if pointer:
   end=self.u32()
   if end==0:return None
   if end<self.p or end>len(self.b):raise ValueError(f'bad end {end:x} at {start:x}')
  cid=self.class_ref()
  self.objects-=1
  data=self.fields(cid,depth)
  if end is not None and self.p!=end:raise ValueError(f'{data["class"]} end mismatch got{self.p:x} expected{end:x}')
  data['start']=start;data['end']=self.p
  return data
 def value(self,t,d):
  if t==1:
   v=struct.unpack('<f',self.take(4))[0]
   if not math.isfinite(v):raise ValueError('float')
   return v
  if t==2:return self.take(8).hex()
  if t==3:return self.take(12).hex()
  if t in (4,22):return self.take(16).hex()
  if t==14:return self.take(64).hex()
  if t==6:return [self.u32(),self.u32()]
  if t==9:return self.u32()
  if t==10:
   v=self.take(1)[0]
   if v not in (0,255):raise ValueError(f'bool {v}')
   return v==255
  if t in (12,15,21):return struct.unpack('<Q',self.take(8))[0]
  if t==16:return self.text()
  if t==17:
   count=self.count()
   if count>4096-self.objects:raise ValueError('object cap')
   if not count:return []
   cid=self.class_ref()
   return [self.fields(cid,d) for _ in range(count)]
  if t in (18,20):return self.object(d,True)
  if t==19:
   count=self.count()
   if count>4096-self.objects:raise ValueError('object cap')
   return [self.object(d,True) for _ in range(count)]
  if t==11:return self.u32()
  raise ValueError(f'unresolved type{t}')

class Writer:
 def __init__(self,header,classes):
  if len(classes)>256 or any(len(props)>128 for _,props in classes):raise ValueError('schema cap')
  self.b=bytearray(header[:20]);self.classes=classes;self.seen=[];self.objects=0
 def put(self,data):
  if len(self.b)+len(data)>65536:raise ValueError('dataset cap')
  self.b.extend(data)
 def u32(self,v):self.put(struct.pack('<I',v))
 def text(self,v):
  b=v.encode('utf8')
  if len(b)>4096:raise ValueError('text cap')
  self.u32(len(b));self.put(b)
 def class_ref(self,obj):
  name=obj['class'];cid=next(i for i,c in enumerate(self.classes) if c[0]==name)
  if cid not in self.seen:
   if cid!=len(self.seen):raise ValueError('class definition order')
   self.seen.append(cid);self.u32(0xffffffff);self.text(name);self.u32(len(self.classes[cid][1]))
   for key,t in self.classes[cid][1]:self.text(key);self.u32(t)
  else:self.u32(cid)
  return cid
 def fields(self,obj,cid,depth=0):
  if depth>32:raise ValueError('depth cap')
  self.objects+=1
  if self.objects>4096:raise ValueError('object cap')
  for key,t in self.classes[cid][1]:self.value(t,obj['fields'][key],depth+1)
 def object(self,obj,pointer=True,depth=0):
  if depth>32:raise ValueError('depth cap')
  self.objects+=1
  if self.objects>4096:raise ValueError('object cap')
  start=len(self.b)
  if pointer:
   self.u32(0)
   if obj is None:return
  cid=self.class_ref(obj)
  # This object was already charged above; its fields are serialized directly here.
  for key,t in self.classes[cid][1]:self.value(t,obj['fields'][key],depth+1)
  if pointer:struct.pack_into('<I',self.b,start,len(self.b))
 def value(self,t,v,depth=0):
  if depth>32:raise ValueError('depth cap')
  if t==1:self.put(struct.pack('<f',v))
  elif t in (2,3,4,14,22):
   raw=bytes.fromhex(v)
   expected={2:8,3:12,4:16,14:64,22:16}[t]
   if len(raw)!=expected:raise ValueError(f'invalid opaque{expected} size')
   self.put(raw)
  elif t==6:
   for n in v:self.u32(n)
  elif t==9:self.u32(v)
  elif t==10:self.put(bytes((255 if v else 0,)))
  elif t in (12,15,21):self.put(struct.pack('<Q',v))
  elif t==16:self.text(v)
  elif t==17:
   if len(v)>4096-self.objects:raise ValueError('object cap')
   self.u32(len(v))
   if v:
    cid=self.class_ref(v[0])
    if any(item['class']!=v[0]['class'] for item in v):raise ValueError('heterogeneous inline object array')
    for item in v:self.fields(item,cid,depth+1)
  elif t==19:
   if len(v)>4096-self.objects:raise ValueError('object cap')
   self.u32(len(v))
   for item in v:self.object(item,True,depth+1)
  elif t in (18,20):self.object(v,True,depth+1)
  elif t==11:self.u32(v)
  else:raise ValueError(t)
if __name__=='__main__':
 for path in sys.argv[1:]:
  with Path(path).open('rb') as f:b=f.read(65537)
  r=Reader(b);obj=r.object()
  if r.p!=len(b) or len(r.classes)!=struct.unpack_from('<I',b,16)[0]:raise ValueError('trailing bytes/class count')
  w=Writer(b,r.classes);w.object(obj)
  assert bytes(w.b)==b, path
  print(Path(path).name, 'exact dataset encode/decode',len(b),'bytes')
