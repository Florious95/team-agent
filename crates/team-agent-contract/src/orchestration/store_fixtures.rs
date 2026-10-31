//! Explicit test setup only, including intentionally inconsistent authority rows.
use super::*;

impl ProjectionStore {
    #[doc(hidden)]
    pub fn fixture_put_seat(&mut self, seat: &SeatRecord) -> Result<(), Error> {
        let seat_key = Key::new("seats", seat.identity.seat.as_str());
        let generation = Key::new("generations", seat.identity.seat.as_str());
        let instance = Key::new("instances", seat.identity.instance.as_str());
        let mut tx = self.files.write([seat_key.clone(), generation.clone(), instance.clone()])?;
        tx.put(seat_key, seat)?;
        tx.put(generation, &seat.identity.generation.0)?;
        tx.put(instance, &seat.identity)?;
        tx.commit()
    }

    #[doc(hidden)]
    pub fn fixture_put_operation(&mut self, op: &OperationRecord) -> Result<(), Error> {
        let key = Key::new("operations", op.id.as_str());
        let creation = Key::new("creation", op.target.identity.seat.as_str());
        let instance = Key::new("creation_instances", op.target.identity.instance.as_str());
        let mut tx = self.files.write([key.clone(), creation.clone(), instance.clone()])?;
        tx.put(key, op)?;
        for index in [creation, instance] {
            if matches!(op.kind, TransactionKind::Startup | TransactionKind::Restart
                | TransactionKind::NewSeatFullSnapshot | TransactionKind::NativeNewSeat) {
                tx.put(index, &op.id)?;
            } else if tx.get::<OperationId>(&index)?.as_ref() == Some(&op.id) {
                tx.delete(index)?;
            }
        }
        tx.commit()
    }

    #[doc(hidden)]
    pub fn fixture_delete_seat(&mut self, seat: &SeatId) -> Result<(), Error> {
        let key = Key::new("seats", seat.as_str());
        let mut tx = self.files.write([key.clone()])?;
        tx.delete(key)?;
        tx.commit()
    }

    #[doc(hidden)]
    pub fn fixture_set_lease(&mut self, seat: &SeatId, operation: Option<&OperationId>) -> Result<(), Error> {
        let key = Key::new("leases", seat.as_str());
        let mut tx = self.files.write([key.clone()])?;
        match operation {
            Some(operation) => tx.put(key, operation)?,
            None => tx.delete(key)?,
        }
        tx.commit()
    }
}
