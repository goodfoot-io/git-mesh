// payment.rs — fixture source for triad_strong parity test
// Shares tokens with billing.rs (billing_amount, payment_gateway, charge_total)
// and with invoice.rs (payment_terms, invoice_date, due_amount).
// Nothing in all three — so pairwise cohesion is positive for all three pairs.

pub struct PaymentRecord {
    pub billing_amount: f64,      // shared with billing.rs
    pub payment_gateway: String,  // shared with billing.rs
    pub charge_total: f64,        // shared with billing.rs
    pub payment_terms: u32,       // shared with invoice.rs
    pub invoice_date: String,     // shared with invoice.rs
    pub due_amount: f64,          // shared with invoice.rs
    pub payment_ref: String,      // unique to payment.rs
}

impl PaymentRecord {
    pub fn billing_amount_valid(&self) -> bool {
        self.billing_amount > 0.0
    }

    pub fn payment_gateway_active(&self) -> bool {
        !self.payment_gateway.is_empty()
    }

    pub fn charge_total_check(&self) -> bool {
        self.charge_total >= self.billing_amount
    }

    pub fn payment_terms_valid(&self) -> bool {
        self.payment_terms > 0
    }

    pub fn invoice_date_valid(&self) -> bool {
        !self.invoice_date.is_empty()
    }

    pub fn due_amount_check(&self, paid: f64) -> bool {
        paid >= self.due_amount
    }

    pub fn payment_summary(&self) -> String {
        format!("gateway={} terms={} due={}", self.payment_gateway, self.payment_terms, self.due_amount)
    }
}
