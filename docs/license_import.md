# Setting Up Cross-Marketplace License Transfers

Jinx can automatically grant a code to redeem a Jinxxy product when a customer presents a valid Gumroad license.

> [!NOTE]
> Jinxxy now has a 1st-party [Gumroad store importer](https://feedback.jinxxy.com/changelog/gumroad-store-importer) which may be a better fit for you
> ([feature comparison](#comparison-to-jinxxys-gumroad-store-importer)).

To set up license transfers in Jinx, the store owner must take the following steps:

1. Ensure that Jinx's [API key](https://dashboard.jinxxy.com/api-keys) has the `discount_codes_write` permission. Jinx did not originally need this, so existing API keys may be missing it.
2. Ensure your product on Gumroad has license keys enabled. You can enable them retroactively for an existing product. See the Gumroad docs on [setting up a product with license keys](https://gumroad.com/help/article/76-license-keys#Setting-up-a-product-with-license-keys-CG7_h).
3. Find your Gumroad product_id. This is the unique ID of the product, shown in the License key block on your product's content page while editing. Product IDs should look like: `SDGgCnivv6gTTHfVRfUBxQ==`
4. Use Jinx's `/enable_gumroad_transfer <gumroad_product_id> <jinxxy_product>` command to create a transfer link between the Gumroad product and its corresponding Jinxxy product.
5. Use Jinx's  `/list_gumroad_transfers` command to list all configured transfer links and check your work.
6. If you wish to disable transfers later, use `/disable_gumroad_transfer <gumroad_product_id>`

Once setup is done, instruct customers to enter their Gumroad license key into the normal Jinx registration form. It will now accept Gumroad license keys and initiate the transfer process.

## Caveats

Jinx achieves the transfer by granting a single-use 100% discount code. This is the only option available under the current Jinxxy API for a 3rd-party application to perform a license transfer.

> [!WARNING]
> Due to the usage of discount codes, there are a number of caveats you should understand:

- Even though the granted discount code is single-use, **nothing prevents a user from sharing the discount code**.
  This means that if for some reason a user is not interested in owning their Gumroad product on their Jinxxy account, they could give their code to a friend.
- **The discount code does not expire**. A user could hold their unused code for an indefinite time. (If this is problematic please let me know, as I actually do have the ability to change this).
- **Discount codes can be redeemed for any product version**. If you use product versions on Jinxxy, especially if they have different prices, this could be problematic.
- **Jinx does not revoke access on Gumroad**. Customers will have access to the product on both marketplaces.

## Comparison to Jinxxy's Gumroad Store Importer

| Feature                                     | Jinx Bot | Jinxxy.com Importer | Notes                                                                      |
| ------------------------------------------- | -------- | ------------------- | -------------------------------------------------------------------------- |
| user interface                              | Discord  | Web                 |                                                                            |
| Gumroad → Jinxxy license transfer           | ✔️        | ✔️                   |                                                                            |
| creates Jinxxy listing from Gumroad listing | ❌        | ✔️                   |                                                                            |
| 1st-party                                   | ❌        | ✔️                   |                                                                            |
| direct product grant                        | ❌        | ✔️                   | Jinx uses 100% discount codes                                              |
| Emails your Gumroad customers               | ❌        | ✔️                   |                                                                            |
| multi-use                                   | ✔️        | ❌                   | Jinxxy's importer can only be used by a store once                         |
| link products with differing names          | ✔️        | ❌                   | Jinxxy's importer requires Gumroad/Jinxxy product names to match *exactly* |
| revokes Gumroad access after transfer       | ❌        | ❌                   |                                                                            |
