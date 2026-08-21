# Setting Up Cross-Marketplace License Transfers

Jinx can automatically grant a code to redeem a Jinxxy product when a customer presents a valid Gumroad license.

1. Ensure that Jinx's [API key](https://dashboard.jinxxy.com/api-keys) has `discount_codes_write` permission. Jinx did not
originally need this, so existing API keys may be missing it.
2. Find your Gumroad product_id. This is the unique ID of the product, shown in the License key block on your product's content page while editing. Product IDs should look like: `SDGgCnivv6gTTHfVRfUBxQ==`
3. Use `/enable_gumroad_transfer <gumroad_product_id> <jinxxy_product>` to create a transfer link between the Gumroad product and its corresponding Jinxxy product.
4. Use `/list_gumroad_transfers` to list all configured transfer links and check your work.
5. If you wish to disable transfers later, use `/disable_gumroad_transfer <gumroad_product_id>`

Instruct customers to enter their Gumroad license key into the normal Jinx registration form, and the bot will automatically walk them through the transfer process.

## Caveats

The mechanism Jinx uses for this feature is unfortunately limited by both the Gumroad and Jinxxy APIs, so there are a few caveats you should be aware of:

- Jinx achieves the transfer by granting a single-use 100% discount code. It does not have the capability to directly
  grant a license to a Jinxxy account. Even though the code is single-use, **nothing prevents a user from sharing the discount code**.
  This means that if for some reason a user is not interested in owning their Gumroad product on their Jinxxy account, they could give their code to a friend.
- **The discount code does not expire**. A user could hold their unused code for an indefinite time. (If this is problematic please let me know, as I actually do have the ability to change this).
- **Jinx does not revoke access on Gumroad**. Customers will have access to the product on both marketplaces.

In practice these caveats don't matter too much, as malicious customers can always leak files in the classic way versus weird license transfer shenanigans.
