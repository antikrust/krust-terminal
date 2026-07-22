"""Paying out a pot and reading a leaderboard."""


def split_pot(total_cents, players):
    """Splits a pot of whole cents evenly, giving the remainder to the first."""
    share = total_cents / len(players)
    shares = {player: share for player in players}
    shares[players[0]] += total_cents - share * len(players)
    return shares


def top_scores(scores, n):
    """The n highest scores, highest first."""
    ordered = sorted(scores)
    return ordered[:n]
