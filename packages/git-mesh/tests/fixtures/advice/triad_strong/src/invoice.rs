// invoice.rs — fixture source for triad_strong parity test
// Shares tokens with billing.rs (invoice_number, billing_account, account_balance)
// and with payment.rs (payment_terms, invoice_date, due_amount).
// Nothing in all three — so pairwise cohesion between billing+invoice
// and invoice+payment is positive.

pub struct InvoiceEntry {
    pub invoice_number: String,   // shared with billing.rs
    pub billing_account: String,  // shared with billing.rs
    pub account_balance: f64,     // shared with billing.rs
    pub payment_terms: u32,       // shared with payment.rs
    pub invoice_date: String,     // shared with payment.rs
    pub due_amount: f64,          // shared with payment.rs
    pub invoice_ref: String,      // unique to invoice.rs
}

impl InvoiceEntry {
    pub fn validate_invoice_number(&self) -> bool {
        !self.invoice_number.is_empty()
    }

    pub fn billing_account_valid(&self) -> bool {
        !self.billing_account.is_empty()
    }

    pub fn account_balance_check(&self) -> bool {
        self.account_balance >= 0.0
    }

    pub fn payment_terms_valid(&self) -> bool {
        self.payment_terms > 0
    }

    pub fn invoice_date_set(&self) -> bool {
        !self.invoice_date.is_empty()
    }

    pub fn due_amount_positive(&self) -> bool {
        self.due_amount > 0.0
    }

    pub fn invoice_summary(&self) -> String {
        format!("invoice={} account={} due={}", self.invoice_number, self.billing_account, self.due_amount)
    }
}
