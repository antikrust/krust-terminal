"""The original demo, kept so /tmp getting swept never loses it."""


def split_pot(total_cents, players):
    """Splits a pot of whole cents evenly, giving the remainder to the first."""
    if not players:
        return {}
    share = total_cents // len(players)
    shares = {player: share for player in players}
    shares[players[0]] += total_cents - share * len(players)
    return shares


def top_scores(scores, n):
    """The n highest scores, highest first."""
    ordered = sorted(scores, reverse=True)
    return ordered[:n]
