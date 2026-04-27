// billing.rs — fixture source for triad_strong parity test
// Shares tokens with invoice.rs (invoice_number, billing_account) and
// with payment.rs (billing_amount, payment_gateway) — but NOT all three,
// so IDF of those shared tokens is positive (ln(4/3) ≈ 0.288).
// Unique to billing+invoice: invoice_number, billing_account, account_balance
// Unique to billing+payment: billing_amount, payment_gateway, charge_total

pub struct BillingRecord {
    pub invoice_number: String,   // shared with invoice.rs
    pub billing_account: String,  // shared with invoice.rs
    pub account_balance: f64,     // shared with invoice.rs
    pub billing_amount: f64,      // shared with payment.rs
    pub payment_gateway: String,  // shared with payment.rs
    pub charge_total: f64,        // shared with payment.rs
    pub billing_ref: String,      // unique to billing.rs
}

impl BillingRecord {
    pub fn validate_invoice_number(&self) -> bool {
        !self.invoice_number.is_empty()
    }

    pub fn account_balance_check(&self) -> bool {
        self.account_balance >= 0.0
    }

    pub fn billing_account_active(&self) -> bool {
        !self.billing_account.is_empty()
    }

    pub fn billing_amount_valid(&self) -> bool {
        self.billing_amount > 0.0
    }

    pub fn payment_gateway_set(&self) -> bool {
        !self.payment_gateway.is_empty()
    }

    pub fn charge_total_computed(&self) -> f64 {
        self.billing_amount + self.charge_total
    }

    pub fn billing_summary(&self) -> String {
        format!("invoice={} account={} amount={}", self.invoice_number, self.billing_account, self.billing_amount)
    }
}
