# Money and shops

[Design summary](README.md) · Status: built: the purse, coins from kills and pickpocketing, shops that sell, buy and resell, spell lessons that cost money, and Haggling and Appraisal. The crafts are still to come.

Money is the first thing in Riftward that isn't a skill, an attribute or an item: a number in each character's purse, earned by fighting and selling, and spent on goods and spells.

## Coins

- **Three coins: copper, silver and gold.** Ten copper make a silver, and ten silver a gold. Amounts are always said in coins ("3 gold, 4 silver and 7 copper"), never as one big number.
- **A purse, not items.** Each character's money is a count of copper on them (`Purse`), saved in the `coins` column of `characters`. It weighs nothing, holds any amount, and isn't lost on death: a corpse keeps a player's gear, not their money.
- **Showing it:** `inventory` ends with what's in your purse, and `score` shows it beside Karma.
- **Giving it:** `give <amount> <player>`, such as `give 5 silver wren`, `give 2g 5s wren`, or `give 12 wren` (a bare number is copper). The room sees coins change hands, not how many.
- **Testing:** the admin `setcoins <player> <amount>` sets anyone's money.

## Where money comes from and goes

| In | Out |
| --- | --- |
| **Kills:** a mob template's `coins: (low, high)`, in copper, rolled into the mob's own purse when it appears; whatever's left in it is taken by the player who killed it ("You take 3 silver from the town guard.") | **Goods** from shops |
| **Selling** to shops | **Spell lessons** |
| **Pickpocketing** a mob's purse (`steal <someone>`), which leaves less for its killer ([as built](effects.md#every-skill-as-data)) | |
| Later: quests and guild contracts, Performance | Later: lodging, repairs, guild dues, Fence fees |

Today's mobs: a sewer rat carries up to 2 copper, a town guard 5 to 30 copper, and the Rat King 1 gold 5 silver to 4 gold.

## Shops

A shopkeeper is a mob whose template has a `shop`:

```ron
"tinker": (
    name: "Brynn Corlack",
    ...
    shop: (
        sells: ["knife", "hatchet", "lockpicks"],   // item templates, `item` or `zone:item`
        buys: ["anything"],                         // what kinds it buys
    ),
),
```

- **Items have a value** (`value`, in copper) on their template, for a common item of standard quality; [rarity and quality](items.md#quality-and-rarity) multiply it. Only items with a value are bought or sold.
- **Selling to you:** at the item's value. `list` shows a shop's goods and prices; `buy <item>` takes the first that matches. A bought item goes into a free hand, or is set down at your feet if both are full; either way it's real (saved) from that moment.
- **Buying from you:** at a third of the value, at least a copper. `sell <item>` sells something you carry (not worn, and empty if it's a container); `value <item>` asks first.
- **What a shop buys** is a list of kinds: item tags (`herb`, `potion`, `bandage`), or what an item is (`weapon`, `armour`, `food`, `drink`, `container`), or `anything`. A shop that doesn't deal in something says it isn't interested.
- **Resale:** a shop sells again, at full value, what it bought, listed as "(used)", unless it stocks that item anyway or there's only ever one of it. It keeps the 20 most recent. What a shop has bought isn't saved: a restart, or the shopkeeper's death, clears it.
- **Bots** may one day help run a shop, but only within limits the template sets (a price modifier per customer, what to buy); the shop's rules here stay the authority, and a bot never makes an item or a coin. See [Bots](architecture.md#bots).
- **The zone test checks shops:** every item a shop sells must exist and have a value.

### Riftwatch's shops

| Shopkeeper | Where | Sells | Buys |
| --- | --- | --- | --- |
| Narina Greenthorn, the herbalist | Herbalist's Stall | Bandages, healing draughts | Herbs, potions, bandages |
| Harwin Colm, the provisioner | Vendor Stalls | Bread, water, a waterskin, torches, packs and pouches, clothes, leather armour | Food, drink, containers, armour |
| Brynn Corlack, the smith | Tinkers' Corner | Knives, a hatchet, a quarterstaff, a smith's hammer, lockpicks, sheaths, a buckler | Anything with a value |

The herbalist's stall used to put bandages and a healing draught on the floor for free; now she sells them.

### Prices

A first pass, in copper (10 = 1 silver, 100 = 1 gold):

| Kind | Examples |
| --- | --- |
| Food and drink | Bread 3, a flask of water 5, berries 1 |
| Herbs | Yarrow 4, kingsfoil 8, moonpetal 15 |
| Supplies | A torch 3, a bandage 8, a waterskin 10, a backpack 30, a healing draught 50 |
| Weapons | A quarterstaff 10, a knife 20, a hatchet 40, a short sword 100 |
| Armour | A padded jack 50, a leather jerkin 100, studded leather 200, a mail shirt 500, a breastplate 1000; a buckler 50, a round shield 100 |
| Valuables | A rat's tail 1, the Rat King's tooth 200, the silver locket 300 |

## Lessons

- **Skill lessons are free,** so no one is locked out of a way to play.
- **Spell lessons cost money,** as each spell says (`price`, in copper), except each tradition's starter spell: **Mend** (Divine), **Spark** (Hermetic) and **Spirit Grasp** (Shamanic) are free. Bless and Beast Speech cost 5 silver, and Ward 8 silver. `learn` lists each spell with its price, and a teacher won't teach one you can't pay for.

## Trade: Haggling and Appraisal

The Trade tree (Charisma) has two entry skills, both taught by the provisioner and granted by the Merchant's Child background. *Built* (`src/trade.rs`).

- **Haggling** works on its own. The first time a character who knows it deals with a shopkeeper (`list`, `buy`, `sell` or `value`), their Haggling and Charisma are pitted against the shopkeeper's, by opposed roll. How surely they win sets a **deal** for the next ten minutes: a twentieth, a tenth, three twentieths or a fifth of an item's value, for the [blows](attributes.md#the-ratio-rule) from glancing to crushing; lost, nothing. The shares, their words and how long a deal lasts are `trade` in `assets/rules.ron`, with the buying share. ("You haggle with the herbalist, who comes down a fifth.")
- **A deal moves both prices:** buying costs the value less the deal, and selling pays a third of the value plus the deal. At the best deal that's 80% and 53%, so the two never cross and nothing can be bought low and sold high. `list` and `value` show your prices once you have a deal.
- **Without Haggling,** a character pays list price. Shopkeepers have Haggling of their own: the herbalist 5 (Haggling and Charisma together), the provisioner 8, the tinker 10, so a novice wins about one haggle in nine with the provisioner and gets better with practice. Each haggle practises the skill.
- **Appraisal** is `appraise <item>` (or `use appraisal <item>`), on anything you can reach: what it's worth, against a difficulty of 2 + 2 × log₁₀(value + 1), so bread is about 3, a short sword 6, a breastplate 8. Failing, you can't put a price on it. Something with no value is easy, against 2: nothing a merchant would pay for. It's data, the difficulty a formula in `trade.skill.ron` ([Every skill as data](effects.md#every-skill-as-data)).

## Planned

- **Appraisal of what a shop would pay,** and of hidden qualities, if items get any.
- **Guild rates and better stock** for guild ranks ([Guild perks](quests.md)).
- **A Fence** in the Back Alley, buying what honest shops won't, with Thievery.
- **Stock that runs out and restocks,** for rarer goods.

## Open questions

- [ ] **Should money drop on death,** into the corpse with a player's gear?
- [ ] **Is a third the right buying price,** and should it vary by shop or by how much a shop already holds?
- [ ] **Town guards drop their swords and armour,** which can now be sold: is that a farm worth closing (no value on mob gear, or gear that's marked as the garrison's)?
