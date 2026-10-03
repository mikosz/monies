# Monies import file format

Monies imports bank statements from JSON files in the format below. A file is typically
prepared from a bank's statement (PDF, CSV, …) by hand, by a script or by an AI. This
document describes the format precisely, so it can be given to an AI together with a
statement and the request to convert it.

## Overview

- The file is UTF-8 encoded JSON.
- It contains one or more **statements**. Each statement belongs to exactly one account in
  Monies; transactions of different accounts go into separate statements.
- Each statement has **lines**, one per transaction, in the order of the bank's statement.
- Fields not described here are ignored, so extra information (e.g. balances) does no harm.
- A file is imported only when all of it is valid. Otherwise Monies lists every problem it
  found, e.g. `statement 1, line 14: amount has more than 2 decimal places`, with statements
  and lines numbered from 1.

## File

| Field        | Required | Value                                    |
|--------------|----------|------------------------------------------|
| `format`     | yes      | Always `"monies-import"`.                |
| `version`    | yes      | Always `1`.                              |
| `statements` | yes      | A non-empty array of statements, below.  |

## Statement

| Field      | Required | Value                                                                                                                                                                    |
|------------|----------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `account`  | yes      | The name of the account in Monies, e.g. `"Gold bank"`. Case doesn't matter.                                                                                              |
| `currency` | yes      | The account's currency as a three-letter ISO 4217 code, e.g. `"EUR"`. It must be the account's currency in Monies; this guards against importing into the wrong account. |
| `source`   | no       | Where the lines come from, e.g. the statement's file name. Defaults to the import file's name.                                                                           |
| `lines`    | yes      | A non-empty array of lines, below.                                                                                                                                       |

## Line

| Field       | Required | Value                                                                                                                                                                                   |
|-------------|----------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `date`      | yes      | The booking date in ISO format `YYYY-MM-DD`, e.g. `"2026-09-15"`, whatever the date format of the statement or the computer.                                                            |
| `amount`    | yes      | The amount as the bank shows it: **money going out is negative**, money coming in positive. See below.                                                                                  |
| `text`      | yes      | The bank's description of the transaction, as on the statement, e.g. `"CARD PAYMENT CORNER SHOP 0042"`.                                                                                 |
| `reference` | no       | The bank's id of the transaction, if the statement shows one. It's the most reliable way to recognise transactions imported before. It must be unique within the account.               |
| `name`      | no       | A proposed name for the entry, e.g. `"Groceries"`.                                                                                                                                      |
| `category`  | no       | A proposed category as a path of names separated by dots, from general to specific, e.g. `"food.shop"` or `"bills.utilities.electricity"`. Categories that don't exist yet are created. |

Optional fields may be left out; an empty string counts as left out too.

### Amounts

- Write amounts as JSON **strings** with a dot as the decimal separator and no currency
  symbol or thousands separators, e.g. `"-1234.50"`. Numbers such as `-1234.5` are accepted
  as well, but strings keep the amount exactly as written.
- At most as many decimal places as the currency has: 2 for most currencies (e.g. EUR, PLN,
  USD), none for e.g. JPY, 3 for e.g. KWD.
- Use the bank's sign convention: a card payment of 12.50 is `"-12.50"`, a salary of 7800 is
  `"7800.00"`. Monies itself takes expenses as positive and income as negative, as they're
  typed in; it converts the amounts when importing.

### Name and category

Monies uses a name and a category for every entry; `name` and `category` propose them. Both
are reviewed before anything is added, so a good guess helps, but a missing one is fine.
Without a `name`, a line is named by its `text` until renamed in the review. Lines Monies has
imported before (see below) show the name and category the entry has in Monies instead.
When converting with an AI, it helps to give it the list of categories already in use
(they're suggested when typing a category in Monies).

## Duplicates

Statements may overlap, e.g. a monthly statement and a later one covering the same days, and
transactions may have been typed into Monies by hand already. Monies recognises lines it has
imported before (by `reference`, or by the same date, amount and text) and always skips them;
lines with the same amount as an entry within a few days are marked as possible duplicates to
review. All other lines are marked to be added. So it's fine to import overlapping statements.

## Example

```json
{
  "format": "monies-import",
  "version": 1,
  "statements": [
    {
      "account": "Gold bank",
      "currency": "EUR",
      "source": "gold-bank-statement-2026-09.pdf",
      "lines": [
        { "date": "2026-09-01", "amount": "-950.00", "text": "STANDING ORDER FLAT 12 RENT",
          "reference": "GB2609-0001", "name": "Rent", "category": "bills.rent" },
        { "date": "2026-09-15", "amount": "-12.50", "text": "CARD PAYMENT CORNER SHOP 0042",
          "reference": "GB2609-0002", "name": "Groceries", "category": "food.shop" },
        { "date": "2026-09-25", "amount": "3100.00", "text": "SALARY FICTIONAL WIDGETS LTD",
          "reference": "GB2609-0003", "name": "Salary", "category": "income.salary" },
        { "date": "2026-09-28", "amount": "-27.30", "text": "CARD PAYMENT PAGE TURNER BOOKSHOP 0118" }
      ]
    },
    {
      "account": "Silver bank",
      "currency": "PLN",
      "source": "silver-bank-statement-2026-09.csv",
      "lines": [
        { "date": "2026-09-03", "amount": "-104.88", "text": "CARD PAYMENT GROCER 17",
          "name": "Groceries", "category": "food.shop" }
      ]
    }
  ]
}
```
