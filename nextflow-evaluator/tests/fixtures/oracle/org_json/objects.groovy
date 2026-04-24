import org.json.JSONObject
import org.json.JSONArray
import org.json.JSONTokener

def o = new JSONObject(new JSONTokener('{"a": {"b": [1, 2]}, "c": "x"}'))
def seen = []
o.a.each { inner -> seen << inner.keySet() }
o.put('d', [e: 1])
o.remove('c')
def arr = new JSONArray([1, 'y'])
println([o.toString(), o.has('c'), o['a'].get('b').length(), seen, arr.length(), arr.get(1), o.d.getClass().name, arr.contains('y'), o.toMap()])
