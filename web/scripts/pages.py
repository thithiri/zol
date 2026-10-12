#!/usr/bin/env python3
"""Generates play.html (the games page: deposit, withdraw, the games) and updown.html from coinflip.html,
the one source for the three pages that share src/house-page.ts. Run after editing coinflip.html:

    python3 scripts/pages.py
"""
import re
from pathlib import Path

web = Path(__file__).resolve().parent.parent
src = (web / "coinflip.html").read_text()


def page(name: str, title: str, nav: str, how: str | None = None) -> str:
    s = src.replace('<body data-page="coinflip">', f'<body data-page="{name}">')
    s = s.replace("<title>Zcash coin flip</title>", f"<title>{title}</title>")
    s = s.replace("<h1>Zcash coin flip</h1>", f"<h1>{title}</h1>")
    s = s.replace('<meta property="og:title" content="Zcash coin flip" />', f'<meta property="og:title" content="{title}" />')
    s = s.replace('<meta property="og:url" content="https://zol.cash/coinflip.html" />', f'<meta property="og:url" content="https://zol.cash/{"play" if name == "home" else name}.html" />')
    s = re.sub(r'<nav class="small games">.*?</nav>', f'<nav class="small games">{nav}</nav>', s, flags=re.S)
    if how is not None:
        s = re.sub(r'(<summary>How it works, and what you trust</summary>\s*<ul class="small muted">).*?(</ul>)', lambda m: m.group(1) + how + m.group(2), s, flags=re.S)
    return "<!-- Generated from coinflip.html by scripts/pages.py; edit coinflip.html. -->\n" + s


games = '<a href="/play.html">Games</a> · {} · {} · <a href="/auction.html">Sealed-bid auction</a>'
(web / "play.html").write_text(page(
    "home", "ZOL games",
    '<b>Games</b> · <a href="/coinflip.html">Coin flip</a> · <a href="/updown.html">ZEC Up/Down</a> · <a href="/auction.html">Sealed-bid auction</a>',
    """
          <li>Deposit shielded ZEC from your Zcash wallet: it goes to the app's vault, held by three operators (2 of 3 sign everything), and a Solana ledger keeps your balance.</li>
          <li>Play any of the games with that balance. Each runs in a MagicBlock rollup; the money only moves as the ledger allows.</li>
          <li>Withdraw to the address you gave at your first deposit: the ledger fixes it, and the operators can't send it elsewhere.</li>
        """,
))
(web / "updown.html").write_text(page(
    "updown", "ZEC Up/Down",
    games.format('<a href="/coinflip.html">Coin flip</a>', "<b>ZEC Up/Down</b>"),
    """
          <li>Start a table: you and the house each put in up to 10 stakes, and the table moves into a MagicBlock rollup once. Each bet is one stake (0.001 TAZ) on ZEC/USD going up or down over the next 30 s.</li>
          <li>The price is Pyth's, live in the rollup. A bet settles at a price published within 2 s after its 30 s are up: a right call pays 1.9× (the house's seat pays 0.9 stake), a wrong one pays its stake to the house, an unchanged price refunds it. The page, the house and the rollup all settle at once; if none manages within that window, the bet is void and nothing moves, so nobody can wait for a better price.</li>
          <li>The table returns to Solana when you stop, run out, or leave it idle; the payout to your balance happens as it returns.</li>
        """,
))
print("wrote play.html, updown.html")
