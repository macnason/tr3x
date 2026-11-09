ObjC.import('AppKit');
function run(argv) {
  const pb = $.NSPasteboard.generalPasteboard;
  const opts = $.NSMutableDictionary.alloc.init;
  opts.setObjectForKey($(true), $.NSPasteboardURLReadingFileURLsOnlyKey);
  const urls = pb.readObjectsForClassesOptions($([$.NSURL]), opts);
  const files = [];
  if (!urls.isNil()) for (let i = 0; i < urls.count; i++) files.push(urls.objectAtIndex(i).path.js);
  if (files.length) return JSON.stringify({ files: files });
  const types = [['public.png', 'png'], ['public.jpeg', 'jpg'], ['org.webmproject.webp', 'webp'],
    ['com.compuserve.gif', 'gif'], ['public.heic', 'heic'], ['public.tiff', 'tiff']];
  for (const [type, ext] of types) {
    const data = pb.dataForType(type);
    if (!data.isNil() && data.length > 0) {
      const path = argv[0] + '/image.' + ext;
      if (!data.writeToFileAtomically(path, true)) return JSON.stringify({});
      return JSON.stringify({ files: [path] });
    }
  }
  const text = pb.stringForType('public.utf8-plain-text');
  return JSON.stringify(text.isNil() ? {} : { text: text.js });
}
