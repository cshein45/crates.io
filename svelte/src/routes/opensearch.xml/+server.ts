import { base } from '$app/paths';

export const prerender = true;

export function GET({ url }) {
  let body = `<?xml version="1.0" encoding="utf-8"?>
<OpenSearchDescription xmlns="http://a9.com/-/spec/opensearch/1.1/">
    <ShortName>crates.io</ShortName>
    <Description>Search for crates in the official Rust package registry</Description>
    <Image type="image/png">${url.origin}${base}/cargo.png</Image>
    <Url type="text/html" method="get" template="${url.origin}${base}/search?q={searchTerms}"/>
</OpenSearchDescription>
`;

  return new Response(body, {
    headers: { 'Content-Type': 'application/opensearchdescription+xml' },
  });
}
