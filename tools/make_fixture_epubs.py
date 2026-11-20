#!/usr/bin/env python3
"""Build the two EPUB fixtures used by src/book.rs import tests.

Standard library only. The books are invented for these tests: every
paragraph is original filler written here, and no real book is described.

  tests/fixtures/epub2-guide.epub    EPUB 2, NCX table of contents, OPF guide
                                     (cover, title-page, toc, text), and a TOC
                                     entry that points at an anchor in the
                                     middle of a shared document.
  tests/fixtures/epub3-landmarks.epub EPUB 3, nav document with landmarks,
                                     epub:type on bodies, one linear="no"
                                     spine item.

Run from anywhere: paths resolve from this file's location.
"""

from pathlib import Path
from zipfile import ZipFile, ZipInfo, ZIP_DEFLATED, ZIP_STORED

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "tests" / "fixtures"

XHTML = """<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>{title}</title></head>
<body{body_attr}>
{body}
</body>
</html>
"""


def page(title: str, body: str, body_attr: str = "") -> str:
    return XHTML.format(title=title, body_attr=body_attr, body=body)


def write_epub(path: Path, files: dict[str, str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with ZipFile(path, "w") as zf:
        # the mimetype entry must come first and stay uncompressed
        zf.writestr(ZipInfo("mimetype"), "application/epub+zip", ZIP_STORED)
        for name, content in files.items():
            zf.writestr(name, content, ZIP_DEFLATED)


def build_epub2() -> None:
    pages = {
        "cover.xhtml": page(
            "Cover",
            "<h1>The Tide Clock</h1>\n<p>A reader fixture by Adela Marsh.</p>",
        ),
        "title.xhtml": page(
            "Title Page",
            "<h1>The Tide Clock</h1>\n<p>Stories from a quiet harbour, told "
            "between two tides.</p>\n<p>First printing.</p>",
        ),
        "toc.xhtml": page(
            "Contents",
            "<h1>Contents</h1>\n<ul><li>Cover</li><li>Title Page</li>"
            "<li>Chapter One</li><li>Chapter Two</li></ul>",
        ),
        "text1.xhtml": page(
            "Chapters",
            "<p>The harbour keeps its own hours, and the keeper of the tide "
            "clock winds it twice a day whether the boats go out or not.</p>\n"
            '<h2 id="c1">Chapter One</h2>\n'
            "<p>Maren inherited the clock from her uncle along with a ledger "
            "of tides written in pencil so soft that each damp autumn blurred "
            "a season away.</p>\n"
            "<p>Each morning she read the dial, noted the water, and decided "
            "which of the two stories the day belonged to.</p>\n"
            '<h2 id="c2">Chapter Two</h2>\n'
            "<p>The winter visitor asked her what the clock was for, and she "
            "said it was for knowing when to stop talking and start listening "
            "to the flats.</p>\n"
            "<p>He stayed until the dial turned, and after that he wrote her "
            "letters that never once mentioned the hour.</p>",
        ),
    }

    opf = """<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>The Tide Clock</dc:title>
    <dc:creator>Adela Marsh</dc:creator>
    <dc:identifier id="bookid">urn:uuid:7c1f0a2e-5b64-4b8e-9b0a-tideclock</dc:identifier>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/>
    <item id="title" href="title.xhtml" media-type="application/xhtml+xml"/>
    <item id="toc-page" href="toc.xhtml" media-type="application/xhtml+xml"/>
    <item id="text1" href="text1.xhtml" media-type="application/xhtml+xml"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
  </manifest>
  <spine toc="ncx">
    <itemref idref="cover"/>
    <itemref idref="title"/>
    <itemref idref="toc-page"/>
    <itemref idref="text1"/>
  </spine>
  <guide>
    <reference type="cover" title="Cover" href="cover.xhtml"/>
    <reference type="title-page" title="Title Page" href="title.xhtml"/>
    <reference type="toc" title="Contents" href="toc.xhtml"/>
    <reference type="text" title="Begin Reading" href="text1.xhtml#c1"/>
  </guide>
</package>
"""
    navpoints = []
    for order, (label, src) in enumerate(
        [
            ("Cover", "cover.xhtml"),
            ("Title Page", "title.xhtml"),
            ("Contents", "toc.xhtml"),
            ("Chapter One", "text1.xhtml#c1"),
            ("Chapter Two", "text1.xhtml#c2"),
        ],
        start=1,
    ):
        navpoints.append(
            f"""    <navPoint id="nav{order}" playOrder="{order}">
      <navLabel><text>{label}</text></navLabel>
      <content src="{src}"/>
    </navPoint>"""
        )
    ncx = f"""<?xml version="1.0" encoding="utf-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="urn:uuid:7c1f0a2e-5b64-4b8e-9b0a-tideclock"/>
    <meta name="dtb:depth" content="1"/>
  </head>
  <docTitle><text>The Tide Clock</text></docTitle>
  <navMap>
{chr(10).join(navpoints)}
  </navMap>
</ncx>
"""
    container = """<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"""
    files = {"META-INF/container.xml": container, "OEBPS/content.opf": opf, "OEBPS/toc.ncx": ncx}
    files.update({f"OEBPS/{name}": text for name, text in pages.items()})
    write_epub(FIXTURES / "epub2-guide.epub", files)


def build_epub3() -> None:
    nav = """<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Navigation</title></head>
<body>
  <nav epub:type="toc" id="toc">
    <h1>Contents</h1>
    <ol>
      <li><a href="cover.xhtml">Cover</a></li>
      <li><a href="dedication.xhtml">Dedication</a></li>
      <li><a href="main1.xhtml">First Part</a></li>
      <li><a href="answers.xhtml">Sidebar Notes</a></li>
    </ol>
  </nav>
  <nav epub:type="landmarks" id="landmarks" hidden="">
    <h2>Guide</h2>
    <ol>
      <li><a epub:type="cover" href="cover.xhtml">Cover</a></li>
      <li><a epub:type="toc" href="nav.xhtml">Table of Contents</a></li>
      <li><a epub:type="bodymatter" href="main1.xhtml">Start of the Story</a></li>
    </ol>
  </nav>
</body>
</html>
"""
    pages = {
        "nav.xhtml": nav,
        "cover.xhtml": page(
            "Cover",
            "<h1>Signal Hill</h1>\n<p>A reader fixture by Adela Marsh.</p>",
        ),
        "dedication.xhtml": page(
            "Dedication",
            "<p>For everyone who climbed the hill before the fog rolled in "
            "and still says it was worth it.</p>",
            ' epub:type="dedication"',
        ),
        "main1.xhtml": page(
            "First Part",
            "<h1>First Part</h1>\n<p>The lamp on Signal Hill blinked in "
            "threes, and the town below set its suppers by the pattern.</p>\n"
            "<p>On clear evenings the keeper logged the boats; on foggy ones "
            "she logged the gulls, who argued all night about the moorings."
            "</p>\n<p>Nothing hurried on the hill, and that was the whole of "
            "its lesson.</p>",
            ' epub:type="bodymatter"',
        ),
        "answers.xhtml": page(
            "Sidebar Notes",
            "<h1>Sidebar Notes</h1>\n<p>Question one: how many lamps? Answer: "
            "one, but it counted itself three times a minute.</p>\n"
            "<p>Question two: who wrote this? The hill, by dictation, on a "
            "day of medium fog.</p>",
        ),
    }
    opf = """<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Signal Hill</dc:title>
    <dc:creator>Adela Marsh</dc:creator>
    <dc:identifier id="bookid">urn:uuid:2b9d6f31-8e47-4c62-9a5d-signalhill</dc:identifier>
    <dc:language>en</dc:language>
    <meta property="dcterms:modified">2026-11-20T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/>
    <item id="dedication" href="dedication.xhtml" media-type="application/xhtml+xml"/>
    <item id="main1" href="main1.xhtml" media-type="application/xhtml+xml"/>
    <item id="answers" href="answers.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="cover"/>
    <itemref idref="dedication"/>
    <itemref idref="main1"/>
    <itemref idref="answers" linear="no"/>
  </spine>
</package>
"""
    container = """<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"""
    files = {"META-INF/container.xml": container, "OEBPS/content.opf": opf}
    files.update({f"OEBPS/{name}": text for name, text in pages.items()})
    write_epub(FIXTURES / "epub3-landmarks.epub", files)


if __name__ == "__main__":
    build_epub2()
    build_epub3()
    for name in ("epub2-guide.epub", "epub3-landmarks.epub"):
        size = (FIXTURES / name).stat().st_size
        print(f"wrote tests/fixtures/{name} ({size} bytes)")
